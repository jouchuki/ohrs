//! WebSocket MCP transport.

/// WebSocket transport for MCP.
pub struct WsTransport {
    url: String,
    headers: std::collections::HashMap<String, String>,
}

impl WsTransport {
    pub fn new(url: String, headers: std::collections::HashMap<String, String>) -> Self {
        Self { url, headers }
    }
}
