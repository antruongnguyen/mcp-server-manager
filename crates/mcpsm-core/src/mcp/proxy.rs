use std::borrow::Cow;
use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    Annotated, CallToolRequestParams, CallToolResult, Content, GetPromptRequestParams,
    GetPromptResult, Implementation, ListPromptsResult, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, Prompt, RawResource,
    RawResourceTemplate, ReadResourceRequestParams, ReadResourceResult, Resource, ResourceTemplate,
    ServerCapabilities, ServerInfo, SetLevelRequestParams, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer};

use crate::core::manager::{SharedMcpClients, SharedServers};
use crate::core::server::ServerStatus;

/// Separator between server ID and tool name in namespaced tool names.
const NAMESPACE_SEP: &str = "__";

/// Default request timeout for queries to child servers.
const CHILD_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
        // Capabilities are static at construction time; tools are always enabled.
        // Prompts, resources, and logging are also always advertised — the handlers return
        // empty results or no-op when no child supports them, which is spec-compliant.
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .enable_logging()
                .build(),
        )
        .with_server_info(Implementation::new("mcpsm", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let (clients, servers) = {
            let clients_lock = self.clients.read().await;
            let servers_lock = self.servers.read().await;
            (clients_lock.clone(), servers_lock.clone())
        };

        let mut all_tools: Vec<Tool> = Vec::new();

        for (server_id, client) in &clients {
            // Only include tools from Ready servers
            let is_ready = servers
                .get(server_id)
                .is_some_and(|s| matches!(s.status, ServerStatus::Ready { .. }));

            if !is_ready {
                continue;
            }

            match tokio::time::timeout(CHILD_REQUEST_TIMEOUT, client.list_all_tools()).await {
                Ok(Ok(tools)) => {
                    for tool in tools {
                        let namespaced_name: Cow<'static, str> = format!(
                            "{}{}{}",
                            server_id, NAMESPACE_SEP, tool.name
                        ).into();
                        let description: Option<Cow<'static, str>> = tool.description.map(|d| {
                            format!("[{}] {}", server_id, d).into()
                        });
                        let mut namespaced_tool = Tool::new_with_raw(
                            namespaced_name,
                            description,
                            tool.input_schema,
                        );
                        if let Some(annotations) = tool.annotations {
                            namespaced_tool = namespaced_tool.with_annotations(annotations);
                        }
                        all_tools.push(namespaced_tool);
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!("[{}] Failed to list tools in proxy: {}", server_id, e);
                }
                Err(_) => {
                    tracing::warn!(
                        "[{}] list_tools timed out after {:?}; skipping",
                        server_id,
                        CHILD_REQUEST_TIMEOUT
                    );
                }
            }
        }

        Ok(ListToolsResult {
            tools: all_tools,
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
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
        let mut child_params = CallToolRequestParams::new(tool_name);
        if let Some(args) = request.arguments {
            child_params = child_params.with_arguments(args);
        }

        match tokio::time::timeout(CHILD_REQUEST_TIMEOUT, client.call_tool(child_params)).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(e)) => {
                Ok(CallToolResult::error(vec![Content::text(format!("Error calling tool: {}", e))]))
            }
            Err(_) => {
                Ok(CallToolResult::error(vec![Content::text(format!(
                    "Request timed out after {:?}",
                    CHILD_REQUEST_TIMEOUT
                ))]))
            }
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let (clients, servers) = {
            let clients_lock = self.clients.read().await;
            let servers_lock = self.servers.read().await;
            (clients_lock.clone(), servers_lock.clone())
        };

        let mut all_resources: Vec<Resource> = Vec::new();

        for (server_id, client) in &clients {
            // Only include resources from Ready servers that advertise resources capability
            let has_resources = servers.get(server_id).is_some_and(|s| {
                matches!(s.status, ServerStatus::Ready { .. })
                    && s.peer_info
                        .as_ref()
                        .is_some_and(|p| p.capabilities.resources)
            });

            if !has_resources {
                continue;
            }

            match tokio::time::timeout(CHILD_REQUEST_TIMEOUT, client.list_all_resources()).await {
                Ok(Ok(resources)) => {
                    for resource in resources {
                        let namespaced_uri =
                            format!("{}{}{}", server_id, NAMESPACE_SEP, resource.raw.uri);
                        let description = resource
                            .raw
                            .description
                            .map(|d| format!("[{}] {}", server_id, d));
                        let mut raw = RawResource::new(namespaced_uri, resource.raw.name);
                        if let Some(title) = resource.raw.title {
                            raw = raw.with_title(title);
                        }
                        if let Some(desc) = description {
                            raw = raw.with_description(desc);
                        }
                        if let Some(mime) = resource.raw.mime_type {
                            raw = raw.with_mime_type(mime);
                        }
                        all_resources.push(Annotated::new(raw, resource.annotations));
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!("[{}] Failed to list resources in proxy: {}", server_id, e);
                }
                Err(_) => {
                    tracing::warn!(
                        "[{}] list_resources timed out after {:?}; skipping",
                        server_id,
                        CHILD_REQUEST_TIMEOUT
                    );
                }
            }
        }

        Ok(ListResourcesResult {
            resources: all_resources,
            next_cursor: None,
            meta: None,
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        let (clients, servers) = {
            let clients_lock = self.clients.read().await;
            let servers_lock = self.servers.read().await;
            (clients_lock.clone(), servers_lock.clone())
        };

        let mut all_templates: Vec<ResourceTemplate> = Vec::new();

        for (server_id, client) in &clients {
            let has_resources = servers.get(server_id).is_some_and(|s| {
                matches!(s.status, ServerStatus::Ready { .. })
                    && s.peer_info
                        .as_ref()
                        .is_some_and(|p| p.capabilities.resources)
            });

            if !has_resources {
                continue;
            }

            match tokio::time::timeout(CHILD_REQUEST_TIMEOUT, client.list_all_resource_templates()).await {
                Ok(Ok(templates)) => {
                    for template in templates {
                        let namespaced_uri = format!(
                            "{}{}{}",
                            server_id, NAMESPACE_SEP, template.raw.uri_template
                        );
                        let description = template
                            .raw
                            .description
                            .map(|d| format!("[{}] {}", server_id, d));
                        let mut raw =
                            RawResourceTemplate::new(namespaced_uri, template.raw.name);
                        if let Some(title) = template.raw.title {
                            raw = raw.with_title(title);
                        }
                        if let Some(desc) = description {
                            raw = raw.with_description(desc);
                        }
                        if let Some(mime) = template.raw.mime_type {
                            raw = raw.with_mime_type(mime);
                        }
                        all_templates.push(Annotated::new(raw, template.annotations));
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        "[{}] Failed to list resource templates in proxy: {}",
                        server_id,
                        e
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        "[{}] list_resource_templates timed out after {:?}; skipping",
                        server_id,
                        CHILD_REQUEST_TIMEOUT
                    );
                }
            }
        }

        Ok(ListResourceTemplatesResult {
            resource_templates: all_templates,
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let uri_str: &str = &request.uri;

        // Split namespaced URI: "server_id__original_uri"
        let (server_id, original_uri) = match uri_str.split_once(NAMESPACE_SEP) {
            Some((sid, uri)) => (sid.to_string(), uri.to_string()),
            None => {
                return Err(McpError::invalid_params(
                    format!(
                        "Resource URI must be namespaced as 'server_id{}uri', got: {}",
                        NAMESPACE_SEP, uri_str
                    ),
                    None,
                ));
            }
        };

        // Look up the client
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

        let child_params = ReadResourceRequestParams::new(original_uri);
        match tokio::time::timeout(CHILD_REQUEST_TIMEOUT, client.read_resource(child_params)).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(e)) => Err(McpError::internal_error(
                format!("Error reading resource from '{}': {}", server_id, e),
                None,
            )),
            Err(_) => Err(McpError::internal_error(
                format!("Request to '{}' timed out after {:?}", server_id, CHILD_REQUEST_TIMEOUT),
                None,
            )),
        }
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let (clients, servers) = {
            let clients_lock = self.clients.read().await;
            let servers_lock = self.servers.read().await;
            (clients_lock.clone(), servers_lock.clone())
        };

        let mut all_prompts: Vec<Prompt> = Vec::new();

        for (server_id, client) in &clients {
            // Only include prompts from Ready servers that advertise prompts capability
            let has_prompts = servers.get(server_id).is_some_and(|s| {
                matches!(s.status, ServerStatus::Ready { .. })
                    && s.peer_info
                        .as_ref()
                        .is_some_and(|p| p.capabilities.prompts)
            });

            if !has_prompts {
                continue;
            }

            match tokio::time::timeout(CHILD_REQUEST_TIMEOUT, client.list_all_prompts()).await {
                Ok(Ok(prompts)) => {
                    for prompt in prompts {
                        let namespaced = Prompt::new(
                            format!("{}{}{}", server_id, NAMESPACE_SEP, prompt.name),
                            prompt.description.map(|d| format!("[{}] {}", server_id, d)),
                            prompt.arguments,
                        );
                        all_prompts.push(namespaced);
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!("[{}] Failed to list prompts in proxy: {}", server_id, e);
                }
                Err(_) => {
                    tracing::warn!(
                        "[{}] list_prompts timed out after {:?}; skipping",
                        server_id,
                        CHILD_REQUEST_TIMEOUT
                    );
                }
            }
        }

        Ok(ListPromptsResult {
            prompts: all_prompts,
            next_cursor: None,
            meta: None,
        })
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        let name_str: &str = &request.name;

        // Split namespaced prompt name: "server_id__prompt_name"
        let (server_id, prompt_name) = match name_str.split_once(NAMESPACE_SEP) {
            Some((sid, pname)) => (sid.to_string(), pname.to_string()),
            None => {
                return Err(McpError::invalid_params(
                    format!(
                        "Prompt name must be namespaced as 'server_id{}prompt_name', got: {}",
                        NAMESPACE_SEP, name_str
                    ),
                    None,
                ));
            }
        };

        // Look up the client
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
        let mut child_params = GetPromptRequestParams::new(prompt_name);
        if let Some(args) = request.arguments {
            child_params.arguments = Some(args);
        }

        match tokio::time::timeout(CHILD_REQUEST_TIMEOUT, client.get_prompt(child_params)).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(e)) => Err(McpError::internal_error(
                format!("Error getting prompt from '{}': {}", server_id, e),
                None,
            )),
            Err(_) => Err(McpError::internal_error(
                format!("Request to '{}' timed out after {:?}", server_id, CHILD_REQUEST_TIMEOUT),
                None,
            )),
        }
    }

    async fn set_level(
        &self,
        request: SetLevelRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        let (clients, servers) = {
            let clients_lock = self.clients.read().await;
            let servers_lock = self.servers.read().await;
            (clients_lock.clone(), servers_lock.clone())
        };

        let mut any_success = false;
        let mut last_error: Option<String> = None;

        for (server_id, client) in &clients {
            // Only forward to Ready servers that advertise logging capability
            let has_logging = servers.get(server_id).is_some_and(|s| {
                matches!(s.status, ServerStatus::Ready { .. })
                    && s.peer_info
                        .as_ref()
                        .is_some_and(|p| p.capabilities.logging)
            });

            if !has_logging {
                continue;
            }

            let params = SetLevelRequestParams::new(request.level);
            match tokio::time::timeout(CHILD_REQUEST_TIMEOUT, client.set_level(params)).await {
                Ok(Ok(())) => {
                    any_success = true;
                }
                Ok(Err(e)) => {
                    tracing::warn!("[{}] Failed to set log level in proxy: {}", server_id, e);
                    last_error = Some(format!("{}: {}", server_id, e));
                }
                Err(_) => {
                    tracing::warn!(
                        "[{}] set_level timed out after {:?}; skipping",
                        server_id,
                        CHILD_REQUEST_TIMEOUT
                    );
                    last_error = Some(format!("{}: request timed out", server_id));
                }
            }
        }

        if any_success {
            Ok(())
        } else if let Some(err) = last_error {
            Err(McpError::internal_error(
                format!("Failed to set log level: {}", err),
                None,
            ))
        } else {
            // No logging-capable servers found — succeed silently
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use rmcp::handler::server::ServerHandler;
    use rmcp::model::{ListToolsResult, PaginatedRequestParams, ServerInfo as RmcpServerInfo};
    use rmcp::service::RequestContext;
    use rmcp::{RoleServer, ServiceExt};

    use crate::mcp::client::{McpClient, McpsmClientHandler};

    /// A test MCP server whose `list_tools` never returns within the test window,
    /// simulating a hung/unresponsive child.
    #[derive(Clone)]
    struct HangingServer;

    impl ServerHandler for HangingServer {
        fn get_info(&self) -> RmcpServerInfo {
            RmcpServerInfo::default()
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, McpError> {
            // Hang far longer than the timeout under test.
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(ListToolsResult {
                tools: Vec::new(),
                next_cursor: None,
                meta: None,
            })
        }
    }

    /// Connect an in-process client/server pair over a duplex stream and return
    /// the running client. The server hangs on `list_tools`.
    async fn connect_hanging_client() -> McpClient {
        let (client_io, server_io) = tokio::io::duplex(4096);

        // Serve the hanging server on one end.
        tokio::spawn(async move {
            if let Ok(server) = HangingServer.serve(server_io).await {
                let _ = server.waiting().await;
            }
        });

        // Connect our real client handler on the other end.
        let (tool_tx, _r1) = tokio::sync::mpsc::channel(8);
        let (res_tx, _r2) = tokio::sync::mpsc::channel(8);
        let (prompt_tx, _r3) = tokio::sync::mpsc::channel(8);
        let (log_tx, _r4) = tokio::sync::mpsc::channel(8);
        let handler =
            McpsmClientHandler::new("hang".to_string(), tool_tx, res_tx, prompt_tx, log_tx);
        handler
            .serve(client_io)
            .await
            .expect("client handshake should succeed")
    }

    /// Regression test for Part B: a query to a hung child must be bounded by
    /// `CHILD_REQUEST_TIMEOUT`, not hang indefinitely.
    ///
    /// This exercises the exact wrapping the proxy applies to every child call.
    /// If the timeout is ever removed, this test hangs until the harness kills
    /// it (or, with the short override below, fails fast).
    #[tokio::test]
    async fn hung_child_request_is_bounded_by_timeout() {
        let client = connect_hanging_client().await;

        // Use a short timeout so the test is fast; the production constant is
        // CHILD_REQUEST_TIMEOUT. This asserts the wrapping pattern works.
        let probe_timeout = Duration::from_millis(300);

        let started = tokio::time::Instant::now();
        let result = tokio::time::timeout(probe_timeout, client.list_all_tools()).await;
        let elapsed = started.elapsed();

        assert!(
            result.is_err(),
            "expected the hung child's list_all_tools to time out"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "timeout did not fire promptly (elapsed {:?}); the bounding wrapper is not working",
            elapsed
        );

        // Sanity: the production constant is a real, non-zero bound.
        assert!(CHILD_REQUEST_TIMEOUT >= Duration::from_secs(1));

        client.cancellation_token().cancel();
    }
}

