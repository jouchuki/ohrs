//! WebSocket MCP transport.

/// WebSocket transport for MCP.
pub struct WsTransport {
    // Stored for the not-yet-implemented WebSocket transport (rmcp lacks a stable client).
    #[allow(dead_code)]
    url: String,
    #[allow(dead_code)]
    headers: std::collections::HashMap<String, String>,
}

impl WsTransport {
    pub fn new(url: String, headers: std::collections::HashMap<String, String>) -> Self {
        Self { url, headers }
    }
}
