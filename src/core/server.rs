use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotationInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolAnnotationInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPeerInfo {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub protocol_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    pub capabilities: McpCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCapabilities {
    pub tools: bool,
    pub resources: bool,
    pub prompts: bool,
    pub logging: bool,
}

impl ServerConfig {
    /// Returns true if this is a stdio (command-based) server.
    pub fn is_stdio(&self) -> bool {
        self.command.is_some()
    }

    /// Returns true if this is a remote (URL-based) server.
    pub fn is_remote(&self) -> bool {
        self.url.is_some()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
#[allow(dead_code)]
pub enum ServerStatus {
    Stopped,
    Starting,
    Running { pid: u32 },
    Initializing { pid: Option<u32> },
    Ready { pid: Option<u32> },
    Stopping,
    Error { message: String },
}

impl fmt::Display for ServerStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerStatus::Stopped => write!(f, "Stopped"),
            ServerStatus::Starting => write!(f, "Starting"),
            ServerStatus::Running { pid } => write!(f, "Running (PID {})", pid),
            ServerStatus::Initializing { pid: Some(pid) } => write!(f, "Initializing (PID {})", pid),
            ServerStatus::Initializing { pid: None } => write!(f, "Initializing (remote)"),
            ServerStatus::Ready { pid: Some(pid) } => write!(f, "Ready (PID {})", pid),
            ServerStatus::Ready { pid: None } => write!(f, "Ready (remote)"),
            ServerStatus::Stopping => write!(f, "Stopping"),
            ServerStatus::Error { message } => write!(f, "Error: {}", message),
        }
    }
}

impl ServerStatus {
    pub fn is_running(&self) -> bool {
        matches!(
            self,
            ServerStatus::Running { .. }
                | ServerStatus::Initializing { .. }
                | ServerStatus::Ready { .. }
        )
    }
}
