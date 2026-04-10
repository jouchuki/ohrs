//! HTTP/SSE MCP transport.

/// HTTP transport for MCP.
pub struct HttpTransport {
    url: String,
    headers: std::collections::HashMap<String, String>,
}

impl HttpTransport {
    pub fn new(url: String, headers: std::collections::HashMap<String, String>) -> Self {
        Self { url, headers }
    }
}
