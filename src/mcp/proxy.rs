use std::borrow::Cow;
use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParam, CallToolResult, Content, Implementation,
    ListToolsResult, PaginatedRequestParam, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{Error as McpError, RoleServer};

use crate::core::manager::{SharedMcpClients, SharedServers};
use crate::core::server::ServerStatus;

/// Separator between server ID and tool name in namespaced tool names.
const NAMESPACE_SEP: &str = "__";

/// MCP proxy handler that aggregates tools from all connected child servers.
///
/// Each external client session gets its own clone of this handler (via the
/// `StreamableHttpService` factory). Since all state is in `Arc`s, cloning is cheap.
#[derive(Clone)]
pub struct ProxyHandler {
    servers: SharedServers,
    clients: SharedMcpClients,
}

impl ProxyHandler {
    pub fn new(servers: SharedServers, clients: SharedMcpClients) -> Self {
        Self { servers, clients }
    }
}

impl ServerHandler for ProxyHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .build(),
            server_info: Implementation {
                name: "mcpsm".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            ..Default::default()
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let clients = self.clients.read().await;
        let servers = self.servers.read().await;

        let mut all_tools: Vec<Tool> = Vec::new();

        for (server_id, client) in clients.iter() {
            // Only include tools from Ready servers
            let is_ready = servers
                .get(server_id)
                .is_some_and(|s| matches!(s.status, ServerStatus::Ready { .. }));

            if !is_ready {
                continue;
            }

            match client.list_all_tools().await {
                Ok(tools) => {
                    for tool in tools {
                        let namespaced_name: Cow<'static, str> = format!(
                            "{}{}{}",
                            server_id, NAMESPACE_SEP, tool.name
                        ).into();
                        let description: Option<Cow<'static, str>> = tool.description.map(|d| {
                            format!("[{}] {}", server_id, d).into()
                        });
                        all_tools.push(Tool {
                            name: namespaced_name,
                            description,
                            input_schema: tool.input_schema,
                            annotations: tool.annotations,
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!("[{}] Failed to list tools in proxy: {}", server_id, e);
                }
            }
        }

        Ok(ListToolsResult {
            tools: all_tools,
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let name_str: &str = &request.name;

        // Split namespaced tool name: "server_id__tool_name"
        let (server_id, tool_name) = match name_str.split_once(NAMESPACE_SEP) {
            Some((sid, tname)) => (sid.to_string(), tname.to_string()),
            None => {
                return Err(McpError::invalid_params(
                    format!(
                        "Tool name must be namespaced as 'server_id{}tool_name', got: {}",
                        NAMESPACE_SEP, name_str
                    ),
                    None,
                ));
            }
        };

        // Look up the client; hold the lock only briefly
        let client = {
            let clients = self.clients.read().await;
            match clients.get(&server_id) {
                Some(c) => Arc::clone(c),
                None => {
                    return Err(McpError::invalid_params(
                        format!("Server '{}' not found or not ready", server_id),
                        None,
                    ));
                }
            }
        };

        // Build the child call params (without namespace prefix)
        let child_params = CallToolRequestParam {
            name: tool_name.into(),
            arguments: request.arguments,
        };

        match client.call_tool(child_params).await {
            Ok(result) => Ok(result),
            Err(e) => {
                Ok(CallToolResult {
                    content: vec![Content::text(format!("Error calling tool: {}", e))],
                    is_error: Some(true),
                })
            }
        }
    }
}
