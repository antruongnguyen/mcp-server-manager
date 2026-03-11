use std::collections::HashMap;
use std::process::Stdio;

use rmcp::model::{CallToolRequestParams, CallToolResult, ClientInfo, Implementation, ServerInfo, Tool};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};

use crate::core::server::ServerConfig;

/// A running MCP client service.
/// We use `ClientInfo` as the handler since it implements `ClientHandler`.
pub type McpClient = RunningService<RoleClient, ClientInfo>;

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
    let client_info = ClientInfo::new(
        Default::default(),
        Implementation::new("mcpsm", env!("CARGO_PKG_VERSION")),
    );

    let client = client_info.serve((stdout, stdin)).await?;

    Ok(StdioConnection {
        client,
        child: Some(child),
        stderr,
    })
}

/// Connect to a remote MCP server via Streamable HTTP.
pub async fn connect_http(url: &str, auth_header: Option<&str>) -> anyhow::Result<McpClient> {
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

    let client_info = ClientInfo::new(
        Default::default(),
        Implementation::new("mcpsm", env!("CARGO_PKG_VERSION")),
    );

    let client = client_info.serve(transport).await?;
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
