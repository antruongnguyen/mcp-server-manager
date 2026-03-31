use std::collections::HashMap;
use std::process::Stdio;

use rmcp::handler::client::ClientHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientInfo, GetPromptRequestParams, GetPromptResult,
    Implementation, Prompt, ReadResourceRequestParams, ReadResourceResult, Resource,
    ResourceTemplate, ServerInfo, Tool,
};
use rmcp::service::{NotificationContext, RunningService};
use rmcp::{RoleClient, ServiceExt};

use crate::core::server::ServerConfig;

/// Custom MCP client handler that forwards `notifications/tools/list_changed`
/// from child servers to the manager via a channel.
pub struct McpsmClientHandler {
    client_info: ClientInfo,
    server_id: String,
    tool_refresh_tx: tokio::sync::mpsc::UnboundedSender<String>,
    resource_refresh_tx: tokio::sync::mpsc::UnboundedSender<String>,
    prompt_refresh_tx: tokio::sync::mpsc::UnboundedSender<String>,
}

impl McpsmClientHandler {
    pub fn new(
        server_id: String,
        tool_refresh_tx: tokio::sync::mpsc::UnboundedSender<String>,
        resource_refresh_tx: tokio::sync::mpsc::UnboundedSender<String>,
        prompt_refresh_tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Self {
        Self {
            client_info: ClientInfo::new(
                Default::default(),
                Implementation::new("mcpsm", env!("CARGO_PKG_VERSION")),
            ),
            server_id,
            tool_refresh_tx,
            resource_refresh_tx,
            prompt_refresh_tx,
        }
    }
}

impl ClientHandler for McpsmClientHandler {
    fn get_info(&self) -> ClientInfo {
        self.client_info.clone()
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        tracing::info!(
            "[{}] Received tools/list_changed notification",
            self.server_id
        );
        let _ = self.tool_refresh_tx.send(self.server_id.clone());
    }

    async fn on_resource_list_changed(&self, _context: NotificationContext<RoleClient>) {
        tracing::info!(
            "[{}] Received resources/list_changed notification",
            self.server_id
        );
        let _ = self.resource_refresh_tx.send(self.server_id.clone());
    }

    async fn on_prompt_list_changed(&self, _context: NotificationContext<RoleClient>) {
        tracing::info!(
            "[{}] Received prompts/list_changed notification",
            self.server_id
        );
        let _ = self.prompt_refresh_tx.send(self.server_id.clone());
    }
}

/// A running MCP client service with our custom handler.
pub type McpClient = RunningService<RoleClient, McpsmClientHandler>;

/// Result of connecting via stdio: the running client + optional child process + stderr.
pub struct StdioConnection {
    pub client: McpClient,
    pub child: Option<tokio::process::Child>,
    pub stderr: Option<tokio::process::ChildStderr>,
}

/// Connect to a stdio MCP server by spawning a child process.
///
/// We manually spawn the process (to capture stderr for logging), then pass
/// `(stdout, stdin)` as a transport to rmcp's `serve()` which does the
/// MCP initialize handshake automatically.
///
/// `shell_env` provides the full user shell environment (captured at startup)
/// so child processes can find tools installed via nvm, pyenv, Homebrew, etc.
pub async fn connect_stdio(
    config: &ServerConfig,
    shell_env: &HashMap<String, String>,
    server_id: &str,
    tool_refresh_tx: tokio::sync::mpsc::UnboundedSender<String>,
    resource_refresh_tx: tokio::sync::mpsc::UnboundedSender<String>,
    prompt_refresh_tx: tokio::sync::mpsc::UnboundedSender<String>,
) -> anyhow::Result<StdioConnection> {
    let command = config.command.as_deref()
        .ok_or_else(|| anyhow::anyhow!("No command specified for stdio server"))?;

    let mut cmd = tokio::process::Command::new(command);
    cmd.args(&config.args);
    cmd.env_clear();
    cmd.envs(shell_env.iter());
    // Config env vars override captured shell env
    if !config.env.is_empty() {
        cmd.envs(&config.env);
    }
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn()?;

    let stdin = child.stdin.take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture stdin"))?;
    let stdout = child.stdout.take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture stdout"))?;
    let stderr = child.stderr.take();

    // (ChildStdout, ChildStdin) implements IntoTransport<RoleClient, ...>
    // rmcp's serve() performs the MCP initialize handshake automatically
    let handler = McpsmClientHandler::new(server_id.to_string(), tool_refresh_tx, resource_refresh_tx, prompt_refresh_tx);

    let client = handler.serve((stdout, stdin)).await?;

    Ok(StdioConnection {
        client,
        child: Some(child),
        stderr,
    })
}

/// Connect to a remote MCP server via Streamable HTTP.
pub async fn connect_http(
    url: &str,
    auth_header: Option<&str>,
    server_id: &str,
    tool_refresh_tx: tokio::sync::mpsc::UnboundedSender<String>,
    resource_refresh_tx: tokio::sync::mpsc::UnboundedSender<String>,
    prompt_refresh_tx: tokio::sync::mpsc::UnboundedSender<String>,
) -> anyhow::Result<McpClient> {
    use rmcp::transport::streamable_http_client::{
        StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
    };

    let config = StreamableHttpClientTransportConfig::with_uri(url);
    let config = if let Some(ah) = auth_header {
        config.auth_header(ah)
    } else {
        config
    };
    let transport = StreamableHttpClientTransport::from_config(config);

    let handler = McpsmClientHandler::new(server_id.to_string(), tool_refresh_tx, resource_refresh_tx, prompt_refresh_tx);

    let client = handler.serve(transport).await?;
    Ok(client)
}

/// List all tools from a connected MCP client.
pub async fn list_tools(client: &McpClient) -> anyhow::Result<Vec<Tool>> {
    let tools = client.list_all_tools().await?;
    Ok(tools)
}

/// Get the peer server info (set after MCP handshake).
pub fn peer_info(client: &McpClient) -> Option<ServerInfo> {
    client.peer_info().cloned()
}

/// Call a tool on a connected MCP client.
#[allow(dead_code)]
pub async fn call_tool(
    client: &McpClient,
    name: &str,
    arguments: Option<serde_json::Value>,
) -> anyhow::Result<CallToolResult> {
    let mut params = CallToolRequestParams::new(name.to_string());
    if let Some(args) = arguments.and_then(|v| v.as_object().cloned()) {
        params = params.with_arguments(args);
    }
    let result = client.call_tool(params).await?;
    Ok(result)
}

/// List all resources from a connected MCP client.
pub async fn list_resources(client: &McpClient) -> anyhow::Result<Vec<Resource>> {
    let resources = client.list_all_resources().await?;
    Ok(resources)
}

/// List all resource templates from a connected MCP client.
pub async fn list_resource_templates(client: &McpClient) -> anyhow::Result<Vec<ResourceTemplate>> {
    let templates = client.list_all_resource_templates().await?;
    Ok(templates)
}

/// Read a resource by URI from a connected MCP client.
pub async fn read_resource(client: &McpClient, uri: String) -> anyhow::Result<ReadResourceResult> {
    let params = ReadResourceRequestParams::new(uri);
    let result = client.read_resource(params).await?;
    Ok(result)
}

/// List all prompts from a connected MCP client.
pub async fn list_prompts(client: &McpClient) -> anyhow::Result<Vec<Prompt>> {
    let prompts = client.list_all_prompts().await?;
    Ok(prompts)
}

/// Get a specific prompt by name from a connected MCP client.
pub async fn get_prompt(
    client: &McpClient,
    name: String,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> anyhow::Result<GetPromptResult> {
    let mut params = GetPromptRequestParams::new(name);
    if let Some(args) = arguments {
        params.arguments = Some(args);
    }
    let result = client.get_prompt(params).await?;
    Ok(result)
}
