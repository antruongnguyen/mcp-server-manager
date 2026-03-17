use super::server::ServerConfig;
use std::collections::HashMap;

pub struct ServerTemplate {
    pub name: &'static str,
    #[allow(dead_code)]
    pub description: &'static str,
    pub config: ServerConfig,
}

pub fn builtin_templates() -> Vec<ServerTemplate> {
    vec![
        ServerTemplate {
            name: "Sequential Thinking",
            description: "MCP server for structured sequential thinking",
            config: ServerConfig {
                command: Some("npx".into()),
                args: vec![
                    "-y".into(),
                    "@modelcontextprotocol/server-sequential-thinking".into(),
                ],
                env: HashMap::new(),
                url: None,
                auth_header: None,
                disabled: false,
            },
        },
        ServerTemplate {
            name: "Context7",
            description: "MCP server for up-to-date library documentation",
            config: ServerConfig {
                command: None,
                args: vec![],
                env: HashMap::new(),
                url: Some("https://mcp.context7.com/mcp".into()),
                auth_header: None,
                disabled: false,
            },
        },
        ServerTemplate {
            name: "GitMCP",
            description: "A dynamic Model Context Protocol server that allows AI assistants to fetch real-time documentation and code from any public GitHub repository on demand, ensuring your AI stays grounded with the latest project context without requiring individual server configurations for every library.",
            config: ServerConfig {
                command: None,
                args: vec![],
                env: HashMap::new(),
                url: Some("https://gitmcp.io/mcp".into()),
                auth_header: None,
                disabled: false,
            },
        },
        ServerTemplate {
            name: "Playwright MCP",
            description: "MCP server for fetching real-time documentation and code from any public GitHub repository on demand, ensuring your AI stays grounded with the latest project context without requiring individual server configurations for every library.",
            config: ServerConfig {
                command: Some("npx".into()),
                args: vec![
                    "-y".into(),
                    "@playwright/mcp@latest".into(),
                ],
                env: HashMap::new(),
                url: None,
                auth_header: None,
                disabled: false,
            },
        },
        // command: npx; args: -y chrome-devtools-mcp@latest
        ServerTemplate {
            name: "Chrome DevTools MCP",
            description: "Provides coding agents with programmatic access to Chrome DevTools for comprehensive browser control, inspection, and debugging.",
            config: ServerConfig {
                command: Some("npx".into()),
                args: vec![
                    "-y".into(),
                    "chrome-devtools-mcp@latest".into(),
                ],
                env: HashMap::new(),
                url: None,
                auth_header: None,
                disabled: false,
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_valid() {
        let templates = builtin_templates();
        assert_eq!(templates.len(), 5);
        for t in &templates {
            assert!(!t.name.is_empty());
            assert!(
                t.config.command.is_some() || t.config.url.is_some(),
                "template '{}' must have command or url",
                t.name
            );
        }
    }

    #[test]
    fn templates_serialize_roundtrip() {
        let templates = builtin_templates();
        for t in &templates {
            let json = serde_json::to_string(&t.config).unwrap();
            let parsed: super::super::server::ServerConfig =
                serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, t.config);
        }
    }
}
