use crate::core::server::{LoggingMessageInfo, PromptInfo, ResourceInfo, ResourceTemplateInfo, ServerConfig, ServerStatus, ToolInfo};
use serde::Serialize;

/// Commands sent from the GUI/web to the backend.
#[derive(Debug)]
#[allow(dead_code)]
pub enum AppCommand {
    StartServer { id: String },
    StopServer { id: String },
    StartAllServers,
    StopAllServers,
    AddServer { id: String, config: ServerConfig },
    UpdateServer { id: String, config: ServerConfig },
    DeleteServer { id: String },
    SetServerDisabled { id: String, disabled: bool },
    LoadConfig,
    SaveConfig,
    ReloadConfigIfChanged,
    SetLoggingLevel { id: String, level: String },
    RequestLogs { id: String },
    ClearLogs { id: String },
    Shutdown,
}

/// Events sent from the backend to the GUI/web UI.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum BackendEvent {
    ServerStatusChanged { id: String, status: ServerStatus },
    LogLine { id: String, line: String },
    LogSnapshot { id: String, lines: Vec<String> },
    ConfigLoaded { servers: Vec<(String, ServerConfig)> },
    McpToolsChanged { id: String, tools: Vec<ToolInfo> },
    McpResourcesChanged {
        id: String,
        resources: Vec<ResourceInfo>,
        resource_templates: Vec<ResourceTemplateInfo>,
    },
    McpPromptsChanged {
        id: String,
        prompts: Vec<PromptInfo>,
    },
    McpLoggingMessage {
        id: String,
        message: LoggingMessageInfo,
    },
    McpLoggingLevelChanged {
        id: String,
        level: String,
    },
    McpServerReady { id: String },
    Error { message: String },
    Shutdown,
}
