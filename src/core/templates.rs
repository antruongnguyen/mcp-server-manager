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
                enabled: true,
            },
        },
        ServerTemplate {
            name: "Knowledge Graph Memory",
            description: "MCP server for persistent knowledge graph memory",
            config: ServerConfig {
                command: Some("npx".into()),
                args: vec!["-y".into(), "@modelcontextprotocol/server-memory".into()],
                env: HashMap::from([("MEMORY_FILE_PATH".into(), "memory.json".into())]),
                url: None,
                enabled: true,
            },
        },
        ServerTemplate {
            name: "Context7",
            description: "MCP server for up-to-date library documentation",
            config: ServerConfig {
                command: Some("npx".into()),
                args: vec!["-y".into(), "@upstash/context7-mcp".into()],
                env: HashMap::new(),
                url: None,
                enabled: true,
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
        assert_eq!(templates.len(), 3);
        for t in &templates {
            assert!(!t.name.is_empty());
            assert!(t.config.command.is_some());
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
