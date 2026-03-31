use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::bridge::commands::{AppCommand, BackendEvent};
use crate::core::config;
use crate::core::log_buffer::LogBuffer;
use crate::core::process;
use crate::core::server::{
    McpCapabilities, McpPeerInfo, ResourceInfo, ResourceTemplateInfo, ServerConfig, ServerStatus,
    ToolAnnotationInfo, ToolInfo,
};
use crate::mcp::client::{self, McpClient};

/// Shared server info exposed to the web layer.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServerInfo {
    pub config: ServerConfig,
    pub status: ServerStatus,
    pub tools: Vec<ToolInfo>,
    pub resources: Vec<ResourceInfo>,
    pub resource_templates: Vec<ResourceTemplateInfo>,
    pub disabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_info: Option<McpPeerInfo>,
}

/// Shared state accessible by both the manager and the web server.
pub type SharedServers = Arc<RwLock<HashMap<String, ServerInfo>>>;

/// Shared map of MCP clients for the proxy to use.
pub type SharedMcpClients = Arc<RwLock<HashMap<String, Arc<McpClient>>>>;

/// The backend manager that runs on a tokio thread.
pub struct ServerManager {
    cmd_rx: tokio::sync::mpsc::UnboundedReceiver<AppCommand>,
    evt_tx: tokio::sync::broadcast::Sender<BackendEvent>,
    shared: SharedServers,
    shared_mcp_clients: SharedMcpClients,
    servers: HashMap<String, ManagedServer>,
    /// Channel for receiving log lines from child process stderr.
    log_tx: tokio::sync::mpsc::UnboundedSender<(String, String)>,
    log_rx: tokio::sync::mpsc::UnboundedReceiver<(String, String)>,
    /// Channel for receiving connection results from async connect tasks.
    connect_result_tx: tokio::sync::mpsc::UnboundedSender<ConnectResult>,
    connect_result_rx: tokio::sync::mpsc::UnboundedReceiver<ConnectResult>,
    /// Channel for receiving tool refresh requests from MCP `notifications/tools/list_changed`.
    tool_refresh_tx: tokio::sync::mpsc::UnboundedSender<String>,
    tool_refresh_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    /// Channel for receiving resource refresh requests from MCP `notifications/resources/list_changed`.
    resource_refresh_tx: tokio::sync::mpsc::UnboundedSender<String>,
    resource_refresh_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    /// Watch channel to signal the proxy when tool lists change.
    tool_change_tx: tokio::sync::watch::Sender<()>,
    /// Port the web server is running on (persisted in config saves).
    port: u16,
    /// Tracks the last time we saved config, to suppress watcher events from our own writes.
    last_save_instant: std::time::Instant,
    /// Captured shell environment for child process spawning.
    shell_env: Arc<HashMap<String, String>>,
    /// Periodic health check interval for detecting crashed servers.
    health_check_interval: tokio::time::Interval,
}

struct ManagedServer {
    config: ServerConfig,
    status: ServerStatus,
    tools: Vec<ToolInfo>,
    resources: Vec<ResourceInfo>,
    resource_templates: Vec<ResourceTemplateInfo>,
    peer_info: Option<McpPeerInfo>,
    logs: LogBuffer,
    child: Option<tokio::process::Child>,
}

/// Result of an async MCP connection attempt (carries the client + metadata).
struct ConnectResult {
    id: String,
    result: Result<ConnectSuccess, String>,
}

struct ConnectSuccess {
    client: Arc<McpClient>,
    child: Option<tokio::process::Child>,
    pid: Option<u32>,
    tools: Vec<ToolInfo>,
    resources: Vec<ResourceInfo>,
    resource_templates: Vec<ResourceTemplateInfo>,
    peer_info: Option<McpPeerInfo>,
}

fn send(tx: &tokio::sync::broadcast::Sender<BackendEvent>, event: BackendEvent) {
    let _ = tx.send(event);
}

/// Convert rmcp Tool to our ToolInfo.
fn tool_to_info(tool: &rmcp::model::Tool) -> ToolInfo {
    ToolInfo {
        name: tool.name.to_string(),
        title: tool.title.as_ref().map(|t| t.to_string()),
        description: tool.description.as_ref().map(|d| d.to_string()),
        annotations: tool.annotations.as_ref().map(|a| ToolAnnotationInfo {
            read_only_hint: a.read_only_hint,
            destructive_hint: a.destructive_hint,
            idempotent_hint: a.idempotent_hint,
            open_world_hint: a.open_world_hint,
        }),
    }
}

/// Convert rmcp ServerInfo (InitializeResult) to our McpPeerInfo.
fn server_info_to_peer_info(info: &rmcp::model::ServerInfo) -> McpPeerInfo {
    let impl_info = &info.server_info;
    let name = impl_info.name.clone();
    let version = impl_info.version.clone();
    let title = impl_info.title.clone();
    let description = impl_info.description.clone();
    let instructions = info.instructions.clone();

    let caps = &info.capabilities;
    McpPeerInfo {
        name,
        version,
        title,
        description,
        protocol_version: info.protocol_version.to_string(),
        instructions,
        capabilities: McpCapabilities {
            tools: caps.tools.is_some(),
            resources: caps.resources.is_some(),
            prompts: caps.prompts.is_some(),
            logging: caps.logging.is_some(),
        },
    }
}

/// Convert rmcp Resource to our ResourceInfo.
fn resource_to_info(resource: &rmcp::model::Resource) -> ResourceInfo {
    ResourceInfo {
        uri: resource.uri.clone(),
        name: resource.name.clone(),
        title: resource.title.clone(),
        description: resource.description.clone(),
        mime_type: resource.mime_type.clone(),
    }
}

/// Convert rmcp ResourceTemplate to our ResourceTemplateInfo.
fn resource_template_to_info(template: &rmcp::model::ResourceTemplate) -> ResourceTemplateInfo {
    ResourceTemplateInfo {
        uri_template: template.uri_template.clone(),
        name: template.name.clone(),
        title: template.title.clone(),
        description: template.description.clone(),
        mime_type: template.mime_type.clone(),
    }
}

impl ServerManager {
    pub fn new(
        cmd_rx: tokio::sync::mpsc::UnboundedReceiver<AppCommand>,
        evt_tx: tokio::sync::broadcast::Sender<BackendEvent>,
        shared: SharedServers,
        shared_mcp_clients: SharedMcpClients,
        tool_change_tx: tokio::sync::watch::Sender<()>,
        port: u16,
        shell_env: Arc<HashMap<String, String>>,
    ) -> Self {
        let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel();
        let (connect_result_tx, connect_result_rx) = tokio::sync::mpsc::unbounded_channel();
        let (tool_refresh_tx, tool_refresh_rx) = tokio::sync::mpsc::unbounded_channel();
        let (resource_refresh_tx, resource_refresh_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut health_check_interval =
            tokio::time::interval(std::time::Duration::from_secs(30));
        health_check_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Self {
            cmd_rx,
            evt_tx,
            shared,
            shared_mcp_clients,
            servers: HashMap::new(),
            log_tx,
            log_rx,
            connect_result_tx,
            connect_result_rx,
            tool_refresh_tx,
            tool_refresh_rx,
            resource_refresh_tx,
            resource_refresh_rx,
            tool_change_tx,
            port,
            last_save_instant: std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(10))
                .unwrap_or_else(std::time::Instant::now),
            shell_env,
            health_check_interval,
        }
    }

    /// Main event loop — runs until Shutdown command.
    pub async fn run(&mut self) {
        tracing::info!("ServerManager started");

        // Auto-load config from ~/.config/mcpsm/mcp.json and start enabled servers
        self.auto_load_config().await;

        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    let should_break = self.handle_command(cmd).await;
                    if should_break {
                        break;
                    }
                }
                log = self.log_rx.recv() => {
                    if let Some((id, line)) = log {
                        self.handle_log_line(id, line).await;
                    }
                }
                result = self.connect_result_rx.recv() => {
                    if let Some(result) = result {
                        self.handle_connect_result(result).await;
                    }
                }
                server_id = self.tool_refresh_rx.recv() => {
                    if let Some(id) = server_id {
                        self.handle_tool_refresh(&id).await;
                    }
                }
                server_id = self.resource_refresh_rx.recv() => {
                    if let Some(id) = server_id {
                        self.handle_resource_refresh(&id).await;
                    }
                }
                _ = self.health_check_interval.tick() => {
                    self.run_health_checks().await;
                }
            }
        }
    }

    async fn auto_load_config(&mut self) {
        self.handle_load_config().await;
        // Auto-start all non-disabled servers concurrently
        let ids: Vec<String> = self
            .servers
            .iter()
            .filter(|(_, s)| !s.config.disabled)
            .map(|(id, _)| id.clone())
            .collect();
        self.start_servers_batch(&ids).await;
    }

    async fn handle_command(&mut self, cmd: AppCommand) -> bool {
        match cmd {
            AppCommand::Shutdown => {
                tracing::info!("ServerManager shutting down");
                send(&self.evt_tx, BackendEvent::Shutdown);
                // Small yield to let SSE flush the shutdown event to connected clients
                tokio::task::yield_now().await;
                self.stop_all_servers().await;
                true
            }
            AppCommand::LoadConfig => {
                self.handle_load_config().await;
                false
            }
            AppCommand::SaveConfig => {
                self.handle_save_config();
                false
            }
            AppCommand::ReloadConfigIfChanged => {
                self.handle_reload_config_if_changed().await;
                false
            }
            AppCommand::AddServer { id, config } => {
                self.handle_add_server(id, config).await;
                false
            }
            AppCommand::UpdateServer { id, config } => {
                self.handle_update_server(id, config).await;
                false
            }
            AppCommand::DeleteServer { id } => {
                self.handle_delete_server(&id).await;
                false
            }
            AppCommand::SetServerDisabled { id, disabled } => {
                self.handle_set_disabled(&id, disabled).await;
                false
            }
            AppCommand::StartServer { id } => {
                self.handle_start_server(&id).await;
                false
            }
            AppCommand::StopServer { id } => {
                self.handle_stop_server(&id).await;
                false
            }
            AppCommand::StartAllServers => {
                self.start_all_servers().await;
                false
            }
            AppCommand::StopAllServers => {
                self.stop_all_servers().await;
                false
            }
            AppCommand::RequestLogs { id } => {
                self.handle_request_logs(&id);
                false
            }
            AppCommand::ClearLogs { id } => {
                self.handle_clear_logs(&id);
                false
            }
        }
    }

    async fn handle_log_line(&mut self, id: String, line: String) {
        if let Some(server) = self.servers.get_mut(&id) {
            server.logs.push(line.clone());
        }
        send(&self.evt_tx, BackendEvent::LogLine { id, line });
    }

    async fn handle_connect_result(&mut self, result: ConnectResult) {
        let ConnectResult { id, result } = result;

        match result {
            Ok(success) => {
                if let Some(server) = self.servers.get_mut(&id) {
                    server.tools = success.tools.clone();
                    server.resources = success.resources.clone();
                    server.resource_templates = success.resource_templates.clone();
                    server.peer_info = success.peer_info.clone();
                    server.status = ServerStatus::Ready { pid: success.pid };
                    server.child = success.child;

                    // Store the MCP client in shared state for the proxy
                    self.shared_mcp_clients
                        .write()
                        .await
                        .insert(id.clone(), success.client);

                    self.sync_shared_state().await;
                    let _ = self.tool_change_tx.send(());

                    let pid = success.pid;
                    send(
                        &self.evt_tx,
                        BackendEvent::ServerStatusChanged {
                            id: id.clone(),
                            status: ServerStatus::Ready { pid },
                        },
                    );
                    send(
                        &self.evt_tx,
                        BackendEvent::McpToolsChanged {
                            id: id.clone(),
                            tools: success.tools.clone(),
                        },
                    );
                    if !success.resources.is_empty() || !success.resource_templates.is_empty() {
                        send(
                            &self.evt_tx,
                            BackendEvent::McpResourcesChanged {
                                id: id.clone(),
                                resources: success.resources.clone(),
                                resource_templates: success.resource_templates.clone(),
                            },
                        );
                    }
                    send(
                        &self.evt_tx,
                        BackendEvent::McpServerReady { id: id.clone() },
                    );

                    // Inject lifecycle logs into the server's log buffer
                    if let Some(ref info) = success.peer_info {
                        self.push_log(
                            &id,
                            format!(
                                "[mcpsm] MCP handshake complete: {} v{} (protocol {})",
                                info.name, info.version, info.protocol_version
                            ),
                        );
                    }
                    let resource_count = success.resources.len() + success.resource_templates.len();
                    if let Some(pid_val) = pid {
                        self.push_log(
                            &id,
                            format!(
                                "[mcpsm] Server ready (PID {}), {} tools, {} resources discovered",
                                pid_val,
                                success.tools.len(),
                                resource_count
                            ),
                        );
                    } else {
                        self.push_log(
                            &id,
                            format!(
                                "[mcpsm] Server ready (remote), {} tools, {} resources discovered",
                                success.tools.len(),
                                resource_count
                            ),
                        );
                    }

                    tracing::info!("Server '{}' ready with PID {:?}", id, pid);
                }
            }
            Err(e) => {
                tracing::error!("[{}] MCP connection failed: {}", id, e);
                self.push_log(&id, format!("[mcpsm] Connection failed: {}", e));
                if let Some(server) = self.servers.get_mut(&id) {
                    server.status = ServerStatus::Error {
                        message: format!("MCP init failed: {}", e),
                    };
                }
                self.shared_mcp_clients.write().await.remove(&id);
                self.sync_shared_state().await;
                send(
                    &self.evt_tx,
                    BackendEvent::ServerStatusChanged {
                        id,
                        status: ServerStatus::Error {
                            message: format!("MCP init failed: {}", e),
                        },
                    },
                );
            }
        }
    }

    async fn handle_load_config(&mut self) {
        match config::load() {
            Ok((servers, _doc)) => {
                self.servers.clear();
                self.shared_mcp_clients.write().await.clear();
                let mut loaded = Vec::new();
                for (id, config) in &servers {
                    self.servers.insert(
                        id.clone(),
                        ManagedServer {
                            config: config.clone(),
                            status: ServerStatus::Stopped,
                            tools: Vec::new(),
                            resources: Vec::new(),
                            resource_templates: Vec::new(),
                            peer_info: None,
                            logs: LogBuffer::new(),
                            child: None,
                        },
                    );
                    loaded.push((id.clone(), config.clone()));
                }
                self.sync_shared_state().await;
                send(&self.evt_tx, BackendEvent::ConfigLoaded { servers: loaded });
            }
            Err(e) => {
                send(
                    &self.evt_tx,
                    BackendEvent::Error {
                        message: format!("Failed to load config: {}", e),
                    },
                );
            }
        }
    }

    fn handle_save_config(&mut self) {
        let existing_doc = match config::load() {
            Ok((_, doc)) => doc,
            Err(_) => serde_json::json!({}),
        };

        let servers: Vec<(String, ServerConfig)> = self
            .servers
            .iter()
            .map(|(id, s)| (id.clone(), s.config.clone()))
            .collect();

        match config::save(&servers, &existing_doc, self.port) {
            Ok(()) => {
                self.last_save_instant = std::time::Instant::now();
                tracing::info!("Config saved ({} servers)", servers.len());
            }
            Err(e) => {
                tracing::error!("Failed to save config: {}", e);
                send(
                    &self.evt_tx,
                    BackendEvent::Error {
                        message: format!("Failed to save config: {}", e),
                    },
                );
            }
        }
    }

    async fn handle_reload_config_if_changed(&mut self) {
        // Ignore events triggered by our own save (within 2 seconds)
        if self.last_save_instant.elapsed() < std::time::Duration::from_secs(2) {
            tracing::debug!("Ignoring config watcher event (recent save)");
            return;
        }

        let (new_servers, _doc) = match config::load() {
            Ok(result) => result,
            Err(e) => {
                tracing::error!("Failed to reload config: {}", e);
                return;
            }
        };

        tracing::info!("Config file changed, reloading...");

        let new_map: HashMap<String, ServerConfig> = new_servers.into_iter().collect();
        let old_ids: Vec<String> = self.servers.keys().cloned().collect();

        // Removed servers: stop and remove
        for id in &old_ids {
            if !new_map.contains_key(id) {
                tracing::info!("Config reload: removing server '{}'", id);
                self.handle_stop_server(id).await;
                self.servers.remove(id);
                self.shared_mcp_clients.write().await.remove(id);
            }
        }

        // New or changed servers
        for (id, new_config) in &new_map {
            if let Some(existing) = self.servers.get(id) {
                // Check if config changed (compare without enabled, then check enabled separately)
                let config_changed = existing.config.command != new_config.command
                    || existing.config.args != new_config.args
                    || existing.config.env != new_config.env
                    || existing.config.url != new_config.url
                    || existing.config.auth_header != new_config.auth_header;
                let disabled_changed = existing.config.disabled != new_config.disabled;
                let was_running = existing.status.is_running();

                if config_changed {
                    tracing::info!("Config reload: server '{}' config changed", id);
                    self.handle_stop_server(id).await;
                    if let Some(server) = self.servers.get_mut(id) {
                        server.config = new_config.clone();
                    }
                    if !new_config.disabled {
                        self.handle_start_server(id).await;
                    }
                } else if disabled_changed {
                    tracing::info!(
                        "Config reload: server '{}' disabled changed to {}",
                        id,
                        new_config.disabled
                    );
                    if let Some(server) = self.servers.get_mut(id) {
                        server.config.disabled = new_config.disabled;
                    }
                    // Disabling stops a running server; enabling does NOT auto-start
                    if new_config.disabled && was_running {
                        self.handle_stop_server(id).await;
                    }
                }
                // Unchanged: skip
            } else {
                // New server
                tracing::info!("Config reload: adding server '{}'", id);
                self.servers.insert(
                    id.clone(),
                    ManagedServer {
                        config: new_config.clone(),
                        status: ServerStatus::Stopped,
                        tools: Vec::new(),
                        resources: Vec::new(),
                        resource_templates: Vec::new(),
                        peer_info: None,
                        logs: LogBuffer::new(),
                        child: None,
                    },
                );
                if !new_config.disabled {
                    self.handle_start_server(id).await;
                }
            }
        }

        self.sync_shared_state().await;
        let loaded: Vec<(String, ServerConfig)> = self
            .servers
            .iter()
            .map(|(id, s)| (id.clone(), s.config.clone()))
            .collect();
        send(&self.evt_tx, BackendEvent::ConfigLoaded { servers: loaded });
    }

    async fn handle_add_server(&mut self, id: String, config: ServerConfig) {
        self.servers.insert(
            id.clone(),
            ManagedServer {
                config,
                status: ServerStatus::Stopped,
                tools: Vec::new(),
                resources: Vec::new(),
                resource_templates: Vec::new(),
                peer_info: None,
                logs: LogBuffer::new(),
                child: None,
            },
        );
        self.sync_shared_state().await;
        self.handle_save_config();
        send(
            &self.evt_tx,
            BackendEvent::ServerStatusChanged {
                id,
                status: ServerStatus::Stopped,
            },
        );
    }

    async fn handle_update_server(&mut self, id: String, config: ServerConfig) {
        let was_running = self
            .servers
            .get(&id)
            .is_some_and(|s| s.status.is_running());

        // Stop the server if it was running
        if was_running {
            self.handle_stop_server(&id).await;
        }

        if let Some(server) = self.servers.get_mut(&id) {
            server.config = config.clone();
        }
        self.sync_shared_state().await;
        self.handle_save_config();

        // Restart if it was previously running
        if was_running && !config.disabled {
            self.handle_start_server(&id).await;
        }
    }

    async fn handle_delete_server(&mut self, id: &str) {
        if let Some(mut server) = self.servers.remove(id) {
            if let Some(ref mut child) = server.child {
                process::stop_server(child).await;
            }
        }
        // Remove the MCP client (cancel via token, then drop the Arc)
        if let Some(client) = self.shared_mcp_clients.write().await.remove(id) {
            client.cancellation_token().cancel();
        }
        let _ = self.tool_change_tx.send(());
        self.sync_shared_state().await;
        self.handle_save_config();
    }

    async fn handle_set_disabled(&mut self, id: &str, disabled: bool) {
        let was_running = self
            .servers
            .get(id)
            .is_some_and(|s| s.status.is_running());

        if let Some(server) = self.servers.get_mut(id) {
            server.config.disabled = disabled;
        } else {
            return;
        }

        // Disabling a running server stops it; enabling does NOT auto-start
        if disabled && was_running {
            self.handle_stop_server(id).await;
        }

        self.handle_save_config();
        self.sync_shared_state().await;
    }

    async fn handle_start_server(&mut self, id: &str) {
        if !self.servers.contains_key(id) {
            send(
                &self.evt_tx,
                BackendEvent::Error {
                    message: format!("Server '{}' not found", id),
                },
            );
            return;
        }

        {
            let server = self.servers.get(id).unwrap();
            if server.status.is_running() {
                return;
            }
        }

        // Update status to Starting
        {
            let server = self.servers.get_mut(id).unwrap();
            server.status = ServerStatus::Starting;
            server.logs.clear();
            server.tools.clear();
            server.resources.clear();
            server.resource_templates.clear();
            server.peer_info = None;
        }
        self.sync_shared_state().await;
        send(
            &self.evt_tx,
            BackendEvent::ServerStatusChanged {
                id: id.to_string(),
                status: ServerStatus::Starting,
            },
        );

        let config = self.servers.get(id).unwrap().config.clone();

        if config.is_stdio() {
            let cmd_line = format!(
                "{} {}",
                config.command.as_deref().unwrap_or(""),
                config.args.join(" ")
            );
            self.push_log(id, format!("[mcpsm] Starting server: {}", cmd_line.trim()));
            self.spawn_stdio_connect(id, &config);
        } else if config.is_remote() {
            self.push_log(
                id,
                format!(
                    "[mcpsm] Connecting to remote server: {}",
                    config.url.as_deref().unwrap_or("")
                ),
            );
            self.spawn_remote_connect(id, &config);
        } else {
            let server = self.servers.get_mut(id).unwrap();
            server.status = ServerStatus::Error {
                message: "Server config must have either 'command' or 'url'".into(),
            };
            self.sync_shared_state().await;
            send(
                &self.evt_tx,
                BackendEvent::ServerStatusChanged {
                    id: id.to_string(),
                    status: ServerStatus::Error {
                        message: "Server config must have either 'command' or 'url'".into(),
                    },
                },
            );
            return;
        }

        // Mark as initializing
        {
            let server = self.servers.get_mut(id).unwrap();
            server.status = ServerStatus::Initializing { pid: None };
        }
        self.sync_shared_state().await;
        send(
            &self.evt_tx,
            BackendEvent::ServerStatusChanged {
                id: id.to_string(),
                status: ServerStatus::Initializing { pid: None },
            },
        );
    }

    fn spawn_stdio_connect(&self, id: &str, config: &ServerConfig) {
        let config_clone = config.clone();
        let id_owned = id.to_string();
        let connect_result_tx = self.connect_result_tx.clone();
        let log_tx = self.log_tx.clone();
        let shell_env = self.shell_env.clone();
        let tool_refresh_tx = self.tool_refresh_tx.clone();
        let resource_refresh_tx = self.resource_refresh_tx.clone();

        tokio::spawn(async move {
            let result = async {
                let conn = client::connect_stdio(
                    &config_clone,
                    &shell_env,
                    &id_owned,
                    tool_refresh_tx,
                    resource_refresh_tx,
                )
                .await
                .map_err(|e| format!("Failed to connect: {}", e))?;

                // Set up stderr log reader
                if let Some(stderr) = conn.stderr {
                    process::spawn_stderr_reader(stderr, id_owned.clone(), log_tx);
                }

                let pid = conn.child.as_ref().and_then(|c| process::get_pid(c));

                // Get peer server info
                let peer_info =
                    client::peer_info(&conn.client).map(|info| server_info_to_peer_info(&info));

                // List tools from the initialized client
                let tools = client::list_tools(&conn.client)
                    .await
                    .map_err(|e| format!("Failed to list tools: {}", e))?;
                let tool_infos: Vec<ToolInfo> = tools.iter().map(|t| tool_to_info(t)).collect();

                // List resources and resource templates if capability is advertised
                let has_resources = peer_info.as_ref().is_some_and(|p| p.capabilities.resources);
                let (resource_infos, resource_template_infos) = if has_resources {
                    let resources = client::list_resources(&conn.client)
                        .await
                        .unwrap_or_default();
                    let templates = client::list_resource_templates(&conn.client)
                        .await
                        .unwrap_or_default();
                    (
                        resources.iter().map(|r| resource_to_info(r)).collect(),
                        templates.iter().map(|t| resource_template_to_info(t)).collect(),
                    )
                } else {
                    (Vec::new(), Vec::new())
                };

                Ok(ConnectSuccess {
                    client: Arc::new(conn.client),
                    child: conn.child,
                    pid,
                    tools: tool_infos,
                    resources: resource_infos,
                    resource_templates: resource_template_infos,
                    peer_info,
                })
            }
            .await;

            let _ = connect_result_tx.send(ConnectResult {
                id: id_owned,
                result,
            });
        });
    }

    fn spawn_remote_connect(&self, id: &str, config: &ServerConfig) {
        let url = config.url.clone().unwrap();
        let auth_header = config.auth_header.clone();
        let id_owned = id.to_string();
        let connect_result_tx = self.connect_result_tx.clone();
        let tool_refresh_tx = self.tool_refresh_tx.clone();
        let resource_refresh_tx = self.resource_refresh_tx.clone();

        tokio::spawn(async move {
            let result = async {
                let mcp_client = client::connect_http(
                    &url,
                    auth_header.as_deref(),
                    &id_owned,
                    tool_refresh_tx,
                    resource_refresh_tx,
                )
                .await
                .map_err(|e| format!("Failed to connect: {}", e))?;

                // Get peer server info
                let peer_info =
                    client::peer_info(&mcp_client).map(|info| server_info_to_peer_info(&info));

                let tools = client::list_tools(&mcp_client)
                    .await
                    .map_err(|e| format!("Failed to list tools: {}", e))?;
                let tool_infos: Vec<ToolInfo> = tools.iter().map(|t| tool_to_info(t)).collect();

                // List resources and resource templates if capability is advertised
                let has_resources = peer_info.as_ref().is_some_and(|p| p.capabilities.resources);
                let (resource_infos, resource_template_infos) = if has_resources {
                    let resources = client::list_resources(&mcp_client)
                        .await
                        .unwrap_or_default();
                    let templates = client::list_resource_templates(&mcp_client)
                        .await
                        .unwrap_or_default();
                    (
                        resources.iter().map(|r| resource_to_info(r)).collect(),
                        templates.iter().map(|t| resource_template_to_info(t)).collect(),
                    )
                } else {
                    (Vec::new(), Vec::new())
                };

                Ok(ConnectSuccess {
                    client: Arc::new(mcp_client),
                    child: None,
                    pid: None,
                    tools: tool_infos,
                    resources: resource_infos,
                    resource_templates: resource_template_infos,
                    peer_info,
                })
            }
            .await;

            let _ = connect_result_tx.send(ConnectResult {
                id: id_owned,
                result,
            });
        });
    }

    async fn handle_stop_server(&mut self, id: &str) {
        let should_stop = self
            .servers
            .get(id)
            .is_some_and(|s| s.status.is_running());

        if !should_stop {
            return;
        }

        self.push_log(id, "[mcpsm] Stopping server...".to_string());

        {
            let server = self.servers.get_mut(id).unwrap();
            server.status = ServerStatus::Stopping;
        }
        self.sync_shared_state().await;
        send(
            &self.evt_tx,
            BackendEvent::ServerStatusChanged {
                id: id.to_string(),
                status: ServerStatus::Stopping,
            },
        );

        // Remove and cancel the MCP client from shared state
        if let Some(client) = self.shared_mcp_clients.write().await.remove(id) {
            client.cancellation_token().cancel();
        }

        {
            let server = self.servers.get_mut(id).unwrap();
            // Stop child process if any (stdio servers)
            if let Some(ref mut child) = server.child {
                process::stop_server(child).await;
            }
            server.child = None;
            server.tools.clear();
            server.resources.clear();
            server.resource_templates.clear();
            server.peer_info = None;
            server.status = ServerStatus::Stopped;
        }
        let _ = self.tool_change_tx.send(());
        self.sync_shared_state().await;
        send(
            &self.evt_tx,
            BackendEvent::ServerStatusChanged {
                id: id.to_string(),
                status: ServerStatus::Stopped,
            },
        );

        self.push_log(id, "[mcpsm] Server stopped".to_string());

        tracing::info!("Server '{}' stopped", id);
    }

    async fn stop_all_servers(&mut self) {
        let ids: Vec<String> = self.servers.keys().cloned().collect();
        for id in ids {
            self.handle_stop_server(&id).await;
        }
    }

    async fn start_all_servers(&mut self) {
        let ids: Vec<String> = self
            .servers
            .iter()
            .filter(|(_, s)| !s.config.disabled && !s.status.is_running())
            .map(|(id, _)| id.clone())
            .collect();
        self.start_servers_batch(&ids).await;
    }

    /// Start multiple servers concurrently: all connect tasks are spawned in one pass
    /// with only 2 shared-state syncs total (Starting + Initializing) instead of 2 per server.
    async fn start_servers_batch(&mut self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }

        // Phase 1: Set all to Starting, clear logs/tools, collect configs
        let mut to_start: Vec<(String, ServerConfig)> = Vec::new();
        for id in ids {
            let Some(server) = self.servers.get(id) else {
                continue;
            };
            if server.status.is_running() {
                continue;
            }

            let server = self.servers.get_mut(id).unwrap();
            server.status = ServerStatus::Starting;
            server.logs.clear();
            server.tools.clear();
            server.resources.clear();
            server.resource_templates.clear();
            server.peer_info = None;

            to_start.push((id.clone(), server.config.clone()));

            send(
                &self.evt_tx,
                BackendEvent::ServerStatusChanged {
                    id: id.clone(),
                    status: ServerStatus::Starting,
                },
            );
        }

        if to_start.is_empty() {
            return;
        }

        // Single shared-state sync for all Starting statuses
        self.sync_shared_state().await;

        // Phase 2: Spawn all connect tasks concurrently, set Initializing
        for (id, config) in &to_start {
            if config.is_stdio() {
                let cmd_line = format!(
                    "{} {}",
                    config.command.as_deref().unwrap_or(""),
                    config.args.join(" ")
                );
                self.push_log(id, format!("[mcpsm] Starting server: {}", cmd_line.trim()));
                self.spawn_stdio_connect(id, config);
            } else if config.is_remote() {
                self.push_log(
                    id,
                    format!(
                        "[mcpsm] Connecting to remote server: {}",
                        config.url.as_deref().unwrap_or("")
                    ),
                );
                self.spawn_remote_connect(id, config);
            } else {
                let server = self.servers.get_mut(id).unwrap();
                server.status = ServerStatus::Error {
                    message: "Server config must have either 'command' or 'url'".into(),
                };
                send(
                    &self.evt_tx,
                    BackendEvent::ServerStatusChanged {
                        id: id.clone(),
                        status: ServerStatus::Error {
                            message: "Server config must have either 'command' or 'url'".into(),
                        },
                    },
                );
                continue;
            }

            let server = self.servers.get_mut(id).unwrap();
            server.status = ServerStatus::Initializing { pid: None };

            send(
                &self.evt_tx,
                BackendEvent::ServerStatusChanged {
                    id: id.clone(),
                    status: ServerStatus::Initializing { pid: None },
                },
            );
        }

        // Single shared-state sync for all Initializing statuses
        self.sync_shared_state().await;
    }

    /// Periodic health check: detect crashed/disconnected MCP servers and attempt auto-restart.
    async fn run_health_checks(&mut self) {
        // Collect Ready servers whose MCP client connection has closed
        let failed_ids: Vec<String> = {
            let clients = self.shared_mcp_clients.read().await;
            self.servers
                .iter()
                .filter(|(_, s)| matches!(s.status, ServerStatus::Ready { .. }))
                .filter(|(id, _)| {
                    clients
                        .get(id.as_str())
                        .is_some_and(|c| c.is_closed())
                })
                .map(|(id, _)| id.clone())
                .collect()
        };

        for id in failed_ids {
            tracing::warn!("[{}] Health check: connection closed, marking as error", id);
            self.push_log(
                &id,
                "[mcpsm] Health check failed: connection lost".to_string(),
            );

            // Clean up the dead client
            self.shared_mcp_clients.write().await.remove(&id);
            if let Some(server) = self.servers.get_mut(&id) {
                if let Some(ref mut child) = server.child {
                    process::stop_server(child).await;
                }
                server.child = None;
                server.tools.clear();
                server.resources.clear();
                server.resource_templates.clear();
                server.peer_info = None;
                server.status = ServerStatus::Error {
                    message: "Connection lost (detected by health check)".into(),
                };
            }
            let _ = self.tool_change_tx.send(());
            self.sync_shared_state().await;
            send(
                &self.evt_tx,
                BackendEvent::ServerStatusChanged {
                    id: id.clone(),
                    status: ServerStatus::Error {
                        message: "Connection lost (detected by health check)".into(),
                    },
                },
            );

            // Attempt auto-restart if not disabled
            let is_disabled = self
                .servers
                .get(&id)
                .is_some_and(|s| s.config.disabled);
            if !is_disabled {
                tracing::info!("[{}] Attempting auto-restart after health check failure", id);
                self.push_log(
                    &id,
                    "[mcpsm] Attempting auto-restart...".to_string(),
                );
                self.handle_start_server(&id).await;
            }
        }
    }

    /// Handle a `notifications/tools/list_changed` from a child MCP server.
    /// Re-fetches the tool list and updates shared state + proxy + dashboard.
    async fn handle_tool_refresh(&mut self, id: &str) {
        // Only refresh if the server is Ready
        let is_ready = self
            .servers
            .get(id)
            .is_some_and(|s| matches!(s.status, ServerStatus::Ready { .. }));
        if !is_ready {
            return;
        }

        // Get a clone of the client Arc to avoid holding locks
        let client = {
            let clients = self.shared_mcp_clients.read().await;
            match clients.get(id) {
                Some(c) => Arc::clone(c),
                None => return,
            }
        };

        self.push_log(
            id,
            "[mcpsm] Received tools/list_changed, refreshing tool list...".to_string(),
        );

        match client::list_tools(&client).await {
            Ok(tools) => {
                let tool_infos: Vec<ToolInfo> = tools.iter().map(|t| tool_to_info(t)).collect();
                let count = tool_infos.len();

                if let Some(server) = self.servers.get_mut(id) {
                    server.tools = tool_infos.clone();
                }

                self.sync_shared_state().await;
                let _ = self.tool_change_tx.send(());

                send(
                    &self.evt_tx,
                    BackendEvent::McpToolsChanged {
                        id: id.to_string(),
                        tools: tool_infos,
                    },
                );

                self.push_log(
                    id,
                    format!("[mcpsm] Tool list refreshed: {} tools", count),
                );

                tracing::info!("[{}] Tool list refreshed: {} tools", id, count);
            }
            Err(e) => {
                tracing::warn!("[{}] Failed to refresh tool list: {}", id, e);
                self.push_log(
                    id,
                    format!("[mcpsm] Failed to refresh tool list: {}", e),
                );
            }
        }
    }

    /// Handle a `notifications/resources/list_changed` from a child MCP server.
    /// Re-fetches the resource and template lists and updates shared state + dashboard.
    async fn handle_resource_refresh(&mut self, id: &str) {
        let is_ready = self
            .servers
            .get(id)
            .is_some_and(|s| matches!(s.status, ServerStatus::Ready { .. }));
        if !is_ready {
            return;
        }

        let client = {
            let clients = self.shared_mcp_clients.read().await;
            match clients.get(id) {
                Some(c) => Arc::clone(c),
                None => return,
            }
        };

        self.push_log(
            id,
            "[mcpsm] Received resources/list_changed, refreshing resource list...".to_string(),
        );

        let resources = match client::list_resources(&client).await {
            Ok(r) => r.iter().map(|r| resource_to_info(r)).collect::<Vec<_>>(),
            Err(e) => {
                tracing::warn!("[{}] Failed to refresh resource list: {}", id, e);
                self.push_log(
                    id,
                    format!("[mcpsm] Failed to refresh resource list: {}", e),
                );
                return;
            }
        };

        let templates = match client::list_resource_templates(&client).await {
            Ok(t) => t.iter().map(|t| resource_template_to_info(t)).collect::<Vec<_>>(),
            Err(e) => {
                tracing::warn!("[{}] Failed to refresh resource template list: {}", id, e);
                self.push_log(
                    id,
                    format!("[mcpsm] Failed to refresh resource template list: {}", e),
                );
                return;
            }
        };

        let count = resources.len() + templates.len();

        if let Some(server) = self.servers.get_mut(id) {
            server.resources = resources.clone();
            server.resource_templates = templates.clone();
        }

        self.sync_shared_state().await;

        send(
            &self.evt_tx,
            BackendEvent::McpResourcesChanged {
                id: id.to_string(),
                resources,
                resource_templates: templates,
            },
        );

        self.push_log(
            id,
            format!("[mcpsm] Resource list refreshed: {} resources", count),
        );

        tracing::info!("[{}] Resource list refreshed: {} resources", id, count);
    }

    fn handle_request_logs(&self, id: &str) {
        if let Some(server) = self.servers.get(id) {
            send(
                &self.evt_tx,
                BackendEvent::LogSnapshot {
                    id: id.to_string(),
                    lines: server.logs.lines(),
                },
            );
        }
    }

    fn handle_clear_logs(&mut self, id: &str) {
        if let Some(server) = self.servers.get_mut(id) {
            server.logs.clear();
            send(
                &self.evt_tx,
                BackendEvent::LogSnapshot {
                    id: id.to_string(),
                    lines: Vec::new(),
                },
            );
        }
    }

    /// Push a lifecycle log line into a server's log buffer and broadcast it.
    fn push_log(&mut self, id: &str, line: String) {
        if let Some(server) = self.servers.get_mut(id) {
            server.logs.push(line.clone());
        }
        send(
            &self.evt_tx,
            BackendEvent::LogLine {
                id: id.to_string(),
                line,
            },
        );
    }

    /// Push current server state into the shared Arc<RwLock<...>> for the web layer.
    async fn sync_shared_state(&self) {
        let snapshot: HashMap<String, ServerInfo> = self
            .servers
            .iter()
            .map(|(id, s)| {
                (
                    id.clone(),
                    ServerInfo {
                        config: s.config.clone(),
                        status: s.status.clone(),
                        tools: s.tools.clone(),
                        resources: s.resources.clone(),
                        resource_templates: s.resource_templates.clone(),
                        disabled: s.config.disabled,
                        peer_info: s.peer_info.clone(),
                    },
                )
            })
            .collect();
        *self.shared.write().await = snapshot;
    }
}
