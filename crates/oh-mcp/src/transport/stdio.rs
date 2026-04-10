//! Stdio MCP transport via tokio::process.

use oh_types::mcp::McpStdioServerConfig;

/// Stdio transport for MCP.
pub struct StdioTransport {
    config: McpStdioServerConfig,
    // TODO: child process handle, stdin/stdout streams
}

impl StdioTransport {
    pub fn new(config: McpStdioServerConfig) -> Self {
        Self { config }
    }

    /// Connect to the MCP server via stdio.
    pub async fn connect(&mut self) -> Result<(), super::super::client::McpError> {
        // TODO: Spawn child process, set up stdin/stdout JSON-RPC
        tracing::warn!(command = %self.config.command, "stdio MCP transport not yet implemented");
        Ok(())
    }
}
