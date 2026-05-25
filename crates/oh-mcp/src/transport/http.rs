//! HTTP/SSE MCP transport.

/// HTTP transport for MCP.
pub struct HttpTransport {
    // Stored for the not-yet-implemented HTTP/SSE transport (see client.rs TODO).
    #[allow(dead_code)]
    url: String,
    #[allow(dead_code)]
    headers: std::collections::HashMap<String, String>,
}

impl HttpTransport {
    pub fn new(url: String, headers: std::collections::HashMap<String, String>) -> Self {
        Self { url, headers }
    }
}
