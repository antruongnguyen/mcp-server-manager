use crate::core::server::ServerConfig;
use serde_json::Value;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MCP_SERVERS_KEY: &str = "mcpServers";
const PORT_KEY: &str = "port";
pub const DEFAULT_PORT: u16 = 3456;

/// Return the MCPSM config file path: ~/.config/mcpsm/mcp.json
pub fn config_path() -> io::Result<PathBuf> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Could not determine config directory"))?;
    Ok(config_dir.join("mcpsm").join("mcp.json"))
}

/// Load MCP server configs from the MCPSM config file.
/// Auto-creates an empty config if the file doesn't exist.
pub fn load() -> io::Result<(Vec<(String, ServerConfig)>, Value)> {
    let path = config_path()?;

    // Auto-create config dir + empty file if missing
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let empty_doc = serde_json::json!({ "mcpServers": {} });
        let content = serde_json::to_string_pretty(&empty_doc)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(&path, content)?;
    }

    load_config_from_path(&path)
}

/// Extract the port from a config document, defaulting to DEFAULT_PORT.
pub fn extract_port(doc: &Value) -> u16 {
    doc.get(PORT_KEY)
        .and_then(|v| v.as_u64())
        .and_then(|v| u16::try_from(v).ok())
        .unwrap_or(DEFAULT_PORT)
}

/// Save MCP server configs to the MCPSM config file.
pub fn save(servers: &[(String, ServerConfig)], existing_doc: &Value, port: u16) -> io::Result<()> {
    let path = config_path()?;
    save_config_to_path(&path, servers, existing_doc, port)
}

/// Read MCP server configs from a specific path.
pub fn load_config_from_path(path: &Path) -> io::Result<(Vec<(String, ServerConfig)>, Value)> {
    let content = fs::read_to_string(path)?;
    let doc: Value = serde_json::from_str(&content)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let servers = extract_servers(&doc);
    Ok((servers, doc))
}

/// Extract server configs from a JSON document.
fn extract_servers(doc: &Value) -> Vec<(String, ServerConfig)> {
    let Some(mcp_servers) = doc.get(MCP_SERVERS_KEY).and_then(|v| v.as_object()) else {
        return Vec::new();
    };

    let mut result = Vec::new();
    for (name, value) in mcp_servers {
        if let Ok(config) = serde_json::from_value::<ServerConfig>(value.clone()) {
            result.push((name.clone(), config));
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

/// Save server configs to a specific path.
pub fn save_config_to_path(
    path: &Path,
    servers: &[(String, ServerConfig)],
    existing_doc: &Value,
    port: u16,
) -> io::Result<()> {
    let mut doc = existing_doc.clone();

    // Build the mcpServers object
    let mut mcp_obj = serde_json::Map::new();
    for (name, config) in servers {
        let value = serde_json::to_value(config)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        mcp_obj.insert(name.clone(), value);
    }

    // Merge into existing doc, preserving all other top-level keys
    let obj = doc
        .as_object_mut()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Config root is not an object"))?;
    obj.insert(MCP_SERVERS_KEY.into(), Value::Object(mcp_obj));

    // Persist port if non-default
    if port != DEFAULT_PORT {
        obj.insert(PORT_KEY.into(), Value::Number(port.into()));
    }

    let content = serde_json::to_string_pretty(&doc)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    // Atomic write: write to temp file, then rename
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "Config path has no parent directory")
    })?;
    fs::create_dir_all(parent)?;

    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, content.as_bytes())?;
    fs::rename(&tmp_path, path)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sample_doc() -> Value {
        serde_json::json!({
            "someOtherKey": true,
            "mcpServers": {
                "test-server": {
                    "command": "npx",
                    "args": ["-y", "@example/server"],
                    "env": { "FOO": "bar" }
                }
            },
            "anotherField": 42
        })
    }

    #[test]
    fn extract_servers_parses_correctly() {
        let doc = sample_doc();
        let servers = extract_servers(&doc);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].0, "test-server");
        assert_eq!(servers[0].1.command.as_deref(), Some("npx"));
        assert_eq!(servers[0].1.args, vec!["-y", "@example/server"]);
        assert_eq!(servers[0].1.env.get("FOO").unwrap(), "bar");
    }

    #[test]
    fn extract_servers_empty_when_no_key() {
        let doc = serde_json::json!({"other": 1});
        assert!(extract_servers(&doc).is_empty());
    }

    #[test]
    fn roundtrip_preserves_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        // Write initial doc with extra fields
        let doc = sample_doc();
        fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        // Load
        let (servers, loaded_doc) = load_config_from_path(&path).unwrap();
        assert_eq!(servers.len(), 1);

        // Add a server and save
        let mut updated = servers;
        updated.push((
            "new-server".into(),
            ServerConfig {
                command: Some("node".into()),
                args: vec!["server.js".into()],
                env: HashMap::new(),
                url: None,
                auth_header: None,
                disabled: false,
            },
        ));
        save_config_to_path(&path, &updated, &loaded_doc, DEFAULT_PORT).unwrap();

        // Reload and verify
        let (final_servers, final_doc) = load_config_from_path(&path).unwrap();
        assert_eq!(final_servers.len(), 2);

        // Unknown fields preserved
        assert_eq!(final_doc["someOtherKey"], Value::Bool(true));
        assert_eq!(final_doc["anotherField"], Value::Number(42.into()));
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deep/config.json");
        let doc = serde_json::json!({});
        let servers = vec![(
            "s1".into(),
            ServerConfig {
                command: Some("echo".into()),
                args: vec![],
                env: HashMap::new(),
                url: None,
                auth_header: None,
                disabled: false,
            },
        )];
        save_config_to_path(&path, &servers, &doc, DEFAULT_PORT).unwrap();
        assert!(path.exists());
    }
}
