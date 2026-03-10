use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::bridge::commands::{AppCommand, BackendEvent};
use crate::core::config;
use crate::core::log_buffer::LogBuffer;
use crate::core::process;
use crate::core::server::{ServerConfig, ServerStatus};
use crate::mcp::client::{self, McpClient};

/// Shared server info exposed to the web layer.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServerInfo {
    pub config: ServerConfig,
    pub status: ServerStatus,
    pub tools: Vec<String>,
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
    /// Watch channel to signal the proxy when tool lists change.
    tool_change_tx: tokio::sync::watch::Sender<()>,
}

struct ManagedServer {
    config: ServerConfig,
    status: ServerStatus,
    tools: Vec<String>,
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
    tool_names: Vec<String>,
}

fn send(tx: &tokio::sync::broadcast::Sender<BackendEvent>, event: BackendEvent) {
    let _ = tx.send(event);
}

impl ServerManager {
    pub fn new(
        cmd_rx: tokio::sync::mpsc::UnboundedReceiver<AppCommand>,
        evt_tx: tokio::sync::broadcast::Sender<BackendEvent>,
        shared: SharedServers,
        shared_mcp_clients: SharedMcpClients,
        tool_change_tx: tokio::sync::watch::Sender<()>,
    ) -> Self {
        let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel();
        let (connect_result_tx, connect_result_rx) = tokio::sync::mpsc::unbounded_channel();
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
            tool_change_tx,
        }
    }

    /// Main event loop — runs until Shutdown command.
    pub async fn run(&mut self) {
        tracing::info!("ServerManager started");

        // Auto-load config from ~/.config/mcpsm/mcp.json
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
            }
        }
    }

    async fn auto_load_config(&mut self) {
        self.handle_load_config().await;
    }

    async fn handle_command(&mut self, cmd: AppCommand) -> bool {
        match cmd {
            AppCommand::Shutdown => {
                tracing::info!("ServerManager shutting down");
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
            AppCommand::StartServer { id } => {
                self.handle_start_server(&id).await;
                false
            }
            AppCommand::StopServer { id } => {
                self.handle_stop_server(&id).await;
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
        }
    }

    async fn handle_log_line(&mut self, id: String, line: String) {
        if let Some(server) = self.servers.get_mut(&id) {
            server.logs.push(line.clone());
        }
        send(
            &self.evt_tx,
            BackendEvent::LogLine { id, line },
        );
    }

    async fn handle_connect_result(&mut self, result: ConnectResult) {
        let ConnectResult { id, result } = result;

        match result {
            Ok(success) => {
                if let Some(server) = self.servers.get_mut(&id) {
                    server.tools = success.tool_names.clone();
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
                            tools: success.tool_names,
                        },
                    );
                    send(
                        &self.evt_tx,
                        BackendEvent::McpServerReady { id: id.clone() },
                    );

                    tracing::info!("Server '{}' ready with PID {:?}", id, pid);
                }
            }
            Err(e) => {
                tracing::error!("[{}] MCP connection failed: {}", id, e);
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

    fn handle_save_config(&self) {
        let existing_doc = match config::load() {
            Ok((_, doc)) => doc,
            Err(_) => serde_json::json!({}),
        };

        let servers: Vec<(String, ServerConfig)> = self
            .servers
            .iter()
            .map(|(id, s)| (id.clone(), s.config.clone()))
            .collect();

        match config::save(&servers, &existing_doc) {
            Ok(()) => {
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

    async fn handle_add_server(&mut self, id: String, config: ServerConfig) {
        self.servers.insert(
            id.clone(),
            ManagedServer {
                config,
                status: ServerStatus::Stopped,
                tools: Vec::new(),
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
        if let Some(server) = self.servers.get_mut(&id) {
            server.config = config;
            self.sync_shared_state().await;
            self.handle_save_config();
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
            self.spawn_stdio_connect(id, &config);
        } else if config.is_remote() {
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

        tokio::spawn(async move {
            let result = async {
                let conn = client::connect_stdio(&config_clone).await
                    .map_err(|e| format!("Failed to connect: {}", e))?;

                // Set up stderr log reader
                if let Some(stderr) = conn.stderr {
                    process::spawn_stderr_reader(stderr, id_owned.clone(), log_tx);
                }

                let pid = conn.child.as_ref().and_then(|c| process::get_pid(c));

                // List tools from the initialized client
                let tools = client::list_tools(&conn.client).await
                    .map_err(|e| format!("Failed to list tools: {}", e))?;
                let tool_names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();

                Ok(ConnectSuccess {
                    client: Arc::new(conn.client),
                    child: conn.child,
                    pid,
                    tool_names,
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
        let id_owned = id.to_string();
        let connect_result_tx = self.connect_result_tx.clone();

        tokio::spawn(async move {
            let result = async {
                let mcp_client = client::connect_http(&url).await
                    .map_err(|e| format!("Failed to connect: {}", e))?;

                let tools = client::list_tools(&mcp_client).await
                    .map_err(|e| format!("Failed to list tools: {}", e))?;
                let tool_names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();

                Ok(ConnectSuccess {
                    client: Arc::new(mcp_client),
                    child: None,
                    pid: None,
                    tool_names,
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

        tracing::info!("Server '{}' stopped", id);
    }

    async fn stop_all_servers(&mut self) {
        let ids: Vec<String> = self.servers.keys().cloned().collect();
        for id in ids {
            self.handle_stop_server(&id).await;
        }
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
                    },
                )
            })
            .collect();
        *self.shared.write().await = snapshot;
    }
}
