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
                Err(e) => {
                    tracing::warn!("[{}] Failed to list tools in proxy: {}", server_id, e);
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

        match client.call_tool(child_params).await {
            Ok(result) => Ok(result),
            Err(e) => {
                Ok(CallToolResult::error(vec![Content::text(format!("Error calling tool: {}", e))]))
            }
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let clients = self.clients.read().await;
        let servers = self.servers.read().await;

        let mut all_resources: Vec<Resource> = Vec::new();

        for (server_id, client) in clients.iter() {
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

            match client.list_all_resources().await {
                Ok(resources) => {
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
                Err(e) => {
                    tracing::warn!("[{}] Failed to list resources in proxy: {}", server_id, e);
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
        let clients = self.clients.read().await;
        let servers = self.servers.read().await;

        let mut all_templates: Vec<ResourceTemplate> = Vec::new();

        for (server_id, client) in clients.iter() {
            let has_resources = servers.get(server_id).is_some_and(|s| {
                matches!(s.status, ServerStatus::Ready { .. })
                    && s.peer_info
                        .as_ref()
                        .is_some_and(|p| p.capabilities.resources)
            });

            if !has_resources {
                continue;
            }

            match client.list_all_resource_templates().await {
                Ok(templates) => {
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
                Err(e) => {
                    tracing::warn!(
                        "[{}] Failed to list resource templates in proxy: {}",
                        server_id,
                        e
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
        client.read_resource(child_params).await.map_err(|e| {
            McpError::internal_error(
                format!("Error reading resource from '{}': {}", server_id, e),
                None,
            )
        })
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let clients = self.clients.read().await;
        let servers = self.servers.read().await;

        let mut all_prompts: Vec<Prompt> = Vec::new();

        for (server_id, client) in clients.iter() {
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

            match client.list_all_prompts().await {
                Ok(prompts) => {
                    for prompt in prompts {
                        let namespaced = Prompt::new(
                            format!("{}{}{}", server_id, NAMESPACE_SEP, prompt.name),
                            prompt.description.map(|d| format!("[{}] {}", server_id, d)),
                            prompt.arguments,
                        );
                        all_prompts.push(namespaced);
                    }
                }
                Err(e) => {
                    tracing::warn!("[{}] Failed to list prompts in proxy: {}", server_id, e);
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

        client.get_prompt(child_params).await.map_err(|e| {
            McpError::internal_error(
                format!("Error getting prompt from '{}': {}", server_id, e),
                None,
            )
        })
    }

    async fn set_level(
        &self,
        request: SetLevelRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        let clients = self.clients.read().await;
        let servers = self.servers.read().await;

        let mut any_success = false;
        let mut last_error: Option<String> = None;

        for (server_id, client) in clients.iter() {
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

            let params = SetLevelRequestParams::new(request.level.clone());
            match client.set_level(params).await {
                Ok(()) => {
                    any_success = true;
                }
                Err(e) => {
                    tracing::warn!("[{}] Failed to set log level in proxy: {}", server_id, e);
                    last_error = Some(format!("{}: {}", server_id, e));
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
