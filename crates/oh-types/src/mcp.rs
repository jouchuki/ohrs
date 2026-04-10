//! MCP configuration and state models.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// stdio MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpStdioServerConfig {
    #[serde(default = "stdio_tag", skip_serializing)]
    pub r#type: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub env: Option<HashMap<String, String>>,
    pub cwd: Option<String>,
}

fn stdio_tag() -> String {
    "stdio".into()
}

/// HTTP MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpHttpServerConfig {
    #[serde(default = "http_tag", skip_serializing)]
    pub r#type: String,
    pub url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

fn http_tag() -> String {
    "http".into()
}

/// WebSocket MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpWebSocketServerConfig {
    #[serde(default = "ws_tag", skip_serializing)]
    pub r#type: String,
    pub url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

fn ws_tag() -> String {
    "ws".into()
}

/// Union of MCP server configuration types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum McpServerConfig {
    #[serde(rename = "stdio")]
    Stdio(McpStdioServerConfig),
    #[serde(rename = "http")]
    Http(McpHttpServerConfig),
    #[serde(rename = "ws")]
    WebSocket(McpWebSocketServerConfig),
}

/// Config file shape used by plugins and project files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpJsonConfig {
    #[serde(default, rename = "mcpServers")]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

/// Tool metadata exposed by an MCP server.
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub server_name: String,
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Resource metadata exposed by an MCP server.
#[derive(Debug, Clone)]
pub struct McpResourceInfo {
    pub server_name: String,
    pub name: String,
    pub uri: String,
    pub description: String,
}

/// Connection state for an MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpConnectionState {
    Connected,
    Failed,
    Pending,
    Disabled,
}

/// Runtime status for one MCP server.
#[derive(Debug, Clone)]
pub struct McpConnectionStatus {
    pub name: String,
    pub state: McpConnectionState,
    pub detail: String,
    pub transport: String,
    pub auth_configured: bool,
    pub tools: Vec<McpToolInfo>,
    pub resources: Vec<McpResourceInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_server_config_stdio_serde_roundtrip() {
        let config = McpServerConfig::Stdio(McpStdioServerConfig {
            r#type: "stdio".into(),
            command: "node".into(),
            args: vec!["server.js".into()],
            env: None,
            cwd: Some("/tmp".into()),
        });
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"type\":\"stdio\""));
        let deser: McpServerConfig = serde_json::from_str(&json).unwrap();
        match deser {
            McpServerConfig::Stdio(s) => {
                assert_eq!(s.command, "node");
                assert_eq!(s.args, vec!["server.js"]);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_mcp_server_config_http_serde_roundtrip() {
        let config = McpServerConfig::Http(McpHttpServerConfig {
            r#type: "http".into(),
            url: "https://example.com/mcp".into(),
            headers: HashMap::new(),
        });
        let json = serde_json::to_string(&config).unwrap();
        let deser: McpServerConfig = serde_json::from_str(&json).unwrap();
        match deser {
            McpServerConfig::Http(h) => assert_eq!(h.url, "https://example.com/mcp"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_mcp_server_config_ws_serde_roundtrip() {
        let config = McpServerConfig::WebSocket(McpWebSocketServerConfig {
            r#type: "ws".into(),
            url: "ws://localhost:8080".into(),
            headers: HashMap::new(),
        });
        let json = serde_json::to_string(&config).unwrap();
        let deser: McpServerConfig = serde_json::from_str(&json).unwrap();
        match deser {
            McpServerConfig::WebSocket(w) => assert_eq!(w.url, "ws://localhost:8080"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_mcp_json_config_serde_roundtrip() {
        let config = McpJsonConfig {
            mcp_servers: HashMap::new(),
        };
        let json = serde_json::to_string(&config).unwrap();
        let deser: McpJsonConfig = serde_json::from_str(&json).unwrap();
        assert!(deser.mcp_servers.is_empty());
    }

    #[test]
    fn test_mcp_json_config_default() {
        let config = McpJsonConfig::default();
        assert!(config.mcp_servers.is_empty());
    }

    #[test]
    fn test_mcp_connection_state_serde_roundtrip() {
        for state in [McpConnectionState::Connected, McpConnectionState::Failed, McpConnectionState::Pending, McpConnectionState::Disabled] {
            let json = serde_json::to_string(&state).unwrap();
            let deser: McpConnectionState = serde_json::from_str(&json).unwrap();
            assert_eq!(deser, state);
        }
    }

    #[test]
    fn test_mcp_connection_state_serde_values() {
        assert_eq!(serde_json::to_string(&McpConnectionState::Connected).unwrap(), "\"connected\"");
        assert_eq!(serde_json::to_string(&McpConnectionState::Failed).unwrap(), "\"failed\"");
    }
}
