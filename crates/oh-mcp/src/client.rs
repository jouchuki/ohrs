//! MCP client manager: connects to MCP servers, calls tools, reads resources.

use oh_types::mcp::*;
use opentelemetry::KeyValue;
use std::collections::HashMap;
use std::time::Instant;
use tracing::{info_span, warn, Instrument};

/// Manages connections to multiple MCP servers.
pub struct McpClientManager {
    configs: HashMap<String, McpServerConfig>,
    // Placeholder for actual MCP sessions — will be populated per-transport
    connected: HashMap<String, McpSession>,
}

/// Internal MCP session state.
struct McpSession {
    tools: Vec<McpToolInfo>,
    resources: Vec<McpResourceInfo>,
    state: McpConnectionState,
}

impl McpClientManager {
    pub fn new(configs: HashMap<String, McpServerConfig>) -> Self {
        Self {
            configs,
            connected: HashMap::new(),
        }
    }

    /// Connect to all configured MCP servers.
    pub async fn connect_all(&mut self) -> Vec<McpConnectionStatus> {
        let mut statuses = Vec::new();

        for (name, config) in &self.configs {
            let span = info_span!("mcp_connect", server = %name);
            let status = async {
                let transport = match config {
                    McpServerConfig::Stdio(_) => "stdio",
                    McpServerConfig::Http(_) => "http",
                    McpServerConfig::WebSocket(_) => "ws",
                };

                // TODO: Implement actual MCP protocol connection
                // For now, mark as pending
                warn!(server = %name, transport, "MCP connection not yet implemented");

                McpConnectionStatus {
                    name: name.clone(),
                    state: McpConnectionState::Pending,
                    detail: "not yet implemented".into(),
                    transport: transport.into(),
                    auth_configured: false,
                    tools: Vec::new(),
                    resources: Vec::new(),
                }
            }
            .instrument(span)
            .await;

            statuses.push(status);
        }

        statuses
    }

    /// List all tools across connected servers.
    pub fn list_tools(&self) -> Vec<McpToolInfo> {
        self.connected
            .values()
            .flat_map(|s| s.tools.iter().cloned())
            .collect()
    }

    /// List all resources across connected servers.
    pub fn list_resources(&self) -> Vec<McpResourceInfo> {
        self.connected
            .values()
            .flat_map(|s| s.resources.iter().cloned())
            .collect()
    }

    /// Call a tool on a specific MCP server.
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<String, McpError> {
        let span = info_span!("mcp_call", server = %server_name, tool = %tool_name);
        let start = Instant::now();

        let result = async {
            let _session = self.connected.get(server_name).ok_or_else(|| {
                McpError::NotConnected(server_name.to_string())
            })?;

            // TODO: Implement actual MCP tool invocation
            Err(McpError::NotImplemented)
        }
        .instrument(span)
        .await;

        let elapsed = start.elapsed().as_secs_f64();
        oh_telemetry::MCP_CALL_DURATION.record(
            elapsed,
            &[
                KeyValue::new("server", server_name.to_string()),
                KeyValue::new("tool", tool_name.to_string()),
            ],
        );

        result
    }

    /// Read a resource from a specific MCP server.
    pub async fn read_resource(
        &self,
        server_name: &str,
        uri: &str,
    ) -> Result<String, McpError> {
        let _session = self.connected.get(server_name).ok_or_else(|| {
            McpError::NotConnected(server_name.to_string())
        })?;

        // TODO: Implement actual MCP resource reading
        Err(McpError::NotImplemented)
    }

    /// Get connection statuses.
    pub fn list_statuses(&self) -> Vec<McpConnectionStatus> {
        self.configs
            .iter()
            .map(|(name, config)| {
                let transport = match config {
                    McpServerConfig::Stdio(_) => "stdio",
                    McpServerConfig::Http(_) => "http",
                    McpServerConfig::WebSocket(_) => "ws",
                };
                if let Some(session) = self.connected.get(name) {
                    McpConnectionStatus {
                        name: name.clone(),
                        state: session.state,
                        detail: String::new(),
                        transport: transport.into(),
                        auth_configured: false,
                        tools: session.tools.clone(),
                        resources: session.resources.clone(),
                    }
                } else {
                    McpConnectionStatus {
                        name: name.clone(),
                        state: McpConnectionState::Pending,
                        detail: "not connected".into(),
                        transport: transport.into(),
                        auth_configured: false,
                        tools: Vec::new(),
                        resources: Vec::new(),
                    }
                }
            })
            .collect()
    }

    /// Close all connections.
    pub async fn close(&mut self) {
        self.connected.clear();
    }

    /// Update a server config.
    pub fn update_server_config(&mut self, name: String, config: McpServerConfig) {
        self.configs.insert(name, config);
    }
}

/// MCP errors.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("server not connected: {0}")]
    NotConnected(String),
    #[error("MCP protocol not yet implemented")]
    NotImplemented,
    #[error("transport error: {0}")]
    Transport(String),
}
