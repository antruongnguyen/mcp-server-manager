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
pub async fn connect_stdio(config: &ServerConfig) -> anyhow::Result<StdioConnection> {
    let command = config.command.as_deref()
        .ok_or_else(|| anyhow::anyhow!("No command specified for stdio server"))?;

    let mut cmd = tokio::process::Command::new(command);
    cmd.args(&config.args);
    if !config.env.is_empty() {
        cmd.envs(&config.env);
    }
    augment_path(&mut cmd);
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
pub async fn connect_http(url: &str) -> anyhow::Result<McpClient> {
    use rmcp::transport::StreamableHttpClientTransport;

    let transport = StreamableHttpClientTransport::from_uri(url);

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

/// Add common Node.js paths to PATH so `npx` can be found.
fn augment_path(cmd: &mut tokio::process::Command) {
    let current_path = std::env::var("PATH").unwrap_or_default();
    let extra_paths = [
        "/usr/local/bin",
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
    ];

    let mut paths: Vec<&str> = extra_paths.to_vec();
    for segment in current_path.split(':') {
        if !paths.contains(&segment) {
            paths.push(segment);
        }
    }

    cmd.env("PATH", paths.join(":"));
}
