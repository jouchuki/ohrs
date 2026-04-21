//! MCP client manager: connects to MCP servers, calls tools, reads resources.
//!
//! Uses the `rmcp` crate (official Rust MCP SDK) for the protocol layer.
//!
//! Supported transports:
//!   • stdio  — spawns a child process, communicates over stdin/stdout JSON-RPC
//!   • http   — streamable-HTTP (SSE + POST) via reqwest
//!   • ws     — TODO: rmcp does not yet ship a stable WebSocket client transport;
//!              returns `McpError::NotImplemented` for this variant.

use oh_types::mcp::*;
use opentelemetry::KeyValue;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tracing::{info, info_span, warn, Instrument};

use rmcp::{
    ClientHandler,
    model::{
        CallToolRequestParams, ReadResourceRequestParams, ResourceContents, RawContent,
    },
    service::{Peer, RoleClient, RunningService, serve_client},
    transport::{
        TokioChildProcess,
        streamable_http_client::{StreamableHttpClientTransport, StreamableHttpClientTransportConfig},
    },
};

// ---------------------------------------------------------------------------
// Public manager
// ---------------------------------------------------------------------------

/// Manages connections to multiple MCP servers.
pub struct McpClientManager {
    configs: HashMap<String, McpServerConfig>,
    connected: HashMap<String, McpSession>,
}

/// Internal MCP session state.
struct McpSession {
    tools: Vec<McpToolInfo>,
    resources: Vec<McpResourceInfo>,
    state: McpConnectionState,
    /// Live rmcp peer handle wrapped in a Mutex so `call_tool` / `read_resource`
    /// can use it from `&self` without requiring `&mut self`.
    peer: Arc<Mutex<Peer<RoleClient>>>,
    /// Cancellation token to stop the background service task on drop.
    _cancel: rmcp::service::RunningServiceCancellationToken,
}

// ---------------------------------------------------------------------------
// Minimal client handler: no server-initiated requests in our use-case.
// ---------------------------------------------------------------------------
#[derive(Clone)]
struct NoopHandler;
impl ClientHandler for NoopHandler {}

// ---------------------------------------------------------------------------
// Generic connection helper
// ---------------------------------------------------------------------------

/// Connect via any rmcp transport, run the `initialize` handshake, and
/// collect the initial tool/resource lists. Returns the peer handle and
/// session metadata.
async fn connect_and_init<T, E, A>(
    transport: T,
    server_name: &str,
) -> Result<
    (
        Arc<Mutex<Peer<RoleClient>>>,
        rmcp::service::RunningServiceCancellationToken,
        Vec<McpToolInfo>,
        Vec<McpResourceInfo>,
    ),
    McpError,
>
where
    T: rmcp::transport::IntoTransport<RoleClient, E, A> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
    A: Send + 'static,
{
    let running: RunningService<RoleClient, NoopHandler> =
        serve_client(NoopHandler, transport)
            .await
            .map_err(|e| map_rmcp_error(format!("initialize failed: {e}"), &e.to_string()))?;

    let peer = running.peer().clone();
    let cancel = running.cancellation_token();

    // Keep the service alive in a background task. It will exit when the peer
    // is cancelled or the transport is closed.
    tokio::spawn(async move {
        let _ = running.waiting().await;
    });

    // List all tools (pagination-aware).
    let tools = peer
        .list_all_tools()
        .await
        .map_err(|e| map_rmcp_error(format!("list_tools: {e}"), &e.to_string()))?
        .into_iter()
        .map(|t| McpToolInfo {
            server_name: server_name.to_string(),
            name: t.name.to_string(),
            description: t.description.as_deref().unwrap_or("").to_string(),
            input_schema: serde_json::Value::Object(t.input_schema.as_ref().clone()),
        })
        .collect::<Vec<_>>();

    // List all resources (pagination-aware).
    let resources = peer
        .list_all_resources()
        .await
        .map_err(|e| map_rmcp_error(format!("list_resources: {e}"), &e.to_string()))?
        .into_iter()
        .map(|r| McpResourceInfo {
            server_name: server_name.to_string(),
            name: r.name.clone(),
            uri: r.uri.clone(),
            description: r.description.clone().unwrap_or_default(),
        })
        .collect::<Vec<_>>();

    Ok((Arc::new(Mutex::new(peer)), cancel, tools, resources))
}

// ---------------------------------------------------------------------------
// Transport-specific constructors
// ---------------------------------------------------------------------------

/// Spawn a child process and connect via stdio JSON-RPC.
async fn connect_stdio(
    name: &str,
    cfg: &McpStdioServerConfig,
) -> Result<
    (
        Arc<Mutex<Peer<RoleClient>>>,
        rmcp::service::RunningServiceCancellationToken,
        Vec<McpToolInfo>,
        Vec<McpResourceInfo>,
    ),
    McpError,
> {
    use tokio::process::Command;

    let mut cmd = Command::new(&cfg.command);
    cmd.args(&cfg.args);
    // TokioChildProcess sets stdin/stdout to piped and stderr to inherited by default.

    if let Some(env) = &cfg.env {
        for (k, v) in env {
            cmd.env(k, v);
        }
    }
    if let Some(cwd) = &cfg.cwd {
        cmd.current_dir(cwd);
    }

    // `tokio::process::Command` implements `Into<process_wrap::tokio::CommandWrap>`,
    // so we pass it directly to TokioChildProcess::new().
    let child = TokioChildProcess::new(cmd)
        .map_err(|e| McpError::Transport(format!("spawn '{}': {e}", cfg.command)))?;

    connect_and_init(child, name).await
}

/// Connect to an HTTP MCP server using the streamable-HTTP transport (SSE + POST).
async fn connect_http(
    name: &str,
    cfg: &McpHttpServerConfig,
) -> Result<
    (
        Arc<Mutex<Peer<RoleClient>>>,
        rmcp::service::RunningServiceCancellationToken,
        Vec<McpToolInfo>,
        Vec<McpResourceInfo>,
    ),
    McpError,
> {
    use http::{HeaderName, HeaderValue};
    use std::str::FromStr as _;

    let mut http_cfg = StreamableHttpClientTransportConfig::with_uri(cfg.url.as_str());

    if !cfg.headers.is_empty() {
        let mut header_map = std::collections::HashMap::new();
        for (k, v) in &cfg.headers {
            let name = HeaderName::from_str(k)
                .map_err(|e| McpError::Transport(format!("invalid header name '{k}': {e}")))?;
            let value = HeaderValue::from_str(v)
                .map_err(|e| McpError::Transport(format!("invalid header value for '{k}': {e}")))?;
            header_map.insert(name, value);
        }
        http_cfg = http_cfg.custom_headers(header_map);
    }

    let transport = StreamableHttpClientTransport::from_config(http_cfg);
    connect_and_init(transport, name).await
}

// ---------------------------------------------------------------------------
// McpClientManager implementation
// ---------------------------------------------------------------------------

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

        // Collect names first to avoid borrow issues during mutation.
        let names: Vec<String> = self.configs.keys().cloned().collect();

        for name in names {
            let config = self.configs[&name].clone();
            let span = info_span!("mcp_connect", server = %name);

            let (transport_label, result) = async {
                match &config {
                    McpServerConfig::Stdio(cfg) => ("stdio", connect_stdio(&name, cfg).await),
                    McpServerConfig::Http(cfg) => ("http", connect_http(&name, cfg).await),
                    McpServerConfig::WebSocket(cfg) => {
                        // TODO: rmcp does not yet expose a stable WebSocket client
                        // transport (ws.rs is commented-out upstream). Return
                        // NotImplemented until upstream support lands.
                        warn!(
                            server = %name,
                            url = %cfg.url,
                            "WebSocket MCP transport not yet implemented in rmcp"
                        );
                        ("ws", Err(McpError::NotImplemented("WebSocket transport not yet supported by rmcp".into())))
                    }
                }
            }
            .instrument(span)
            .await;

            let status = match result {
                Ok((peer, cancel, tools, resources)) => {
                    info!(
                        server = %name,
                        transport = transport_label,
                        tools = tools.len(),
                        resources = resources.len(),
                        "MCP server connected"
                    );
                    let session = McpSession {
                        tools: tools.clone(),
                        resources: resources.clone(),
                        state: McpConnectionState::Connected,
                        peer,
                        _cancel: cancel,
                    };
                    self.connected.insert(name.clone(), session);
                    McpConnectionStatus {
                        name: name.clone(),
                        state: McpConnectionState::Connected,
                        detail: String::new(),
                        transport: transport_label.into(),
                        auth_configured: cfg_has_auth(&config),
                        tools,
                        resources,
                    }
                }
                Err(e) => {
                    warn!(server = %name, transport = transport_label, error = %e, "MCP connection failed");
                    McpConnectionStatus {
                        name: name.clone(),
                        state: McpConnectionState::Failed,
                        detail: e.to_string(),
                        transport: transport_label.into(),
                        auth_configured: false,
                        tools: Vec::new(),
                        resources: Vec::new(),
                    }
                }
            };
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
    ///
    /// `arguments` must be a JSON object (`serde_json::Value::Object`) or `null`.
    /// The response is concatenated into a single string for the LLM.
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<String, McpError> {
        let span = info_span!("mcp_call", server = %server_name, tool = %tool_name);
        let start = Instant::now();

        let result = async {
            let session = self
                .connected
                .get(server_name)
                .ok_or_else(|| McpError::NotConnected(server_name.to_string()))?;

            // Validate tool exists in the cached tool list.
            let tool_known = session.tools.iter().any(|t| t.name == tool_name);
            if !tool_known {
                return Err(McpError::ToolNotFound {
                    server: server_name.to_string(),
                    tool: tool_name.to_string(),
                });
            }

            // Build the request params using the builder API (struct is #[non_exhaustive]).
            let params = match arguments {
                serde_json::Value::Object(map) => {
                    CallToolRequestParams::new(tool_name.to_string()).with_arguments(map)
                }
                serde_json::Value::Null => CallToolRequestParams::new(tool_name.to_string()),
                other => {
                    return Err(McpError::Protocol(format!(
                        "call_tool: arguments must be a JSON object or null, got: {other}"
                    )));
                }
            };

            let peer = session.peer.lock().await;
            let call_result = peer
                .call_tool(params)
                .await
                .map_err(|e| map_rmcp_error(format!("call_tool '{tool_name}': {e}"), &e.to_string()))?;

            // Concatenate all text content blocks. Non-text blocks are JSON-serialized
            // so callers always receive a plain string.
            let output = if call_result.content.is_empty() {
                call_result
                    .structured_content
                    .as_ref()
                    .map(|v| serde_json::to_string(v).unwrap_or_default())
                    .unwrap_or_default()
            } else {
                call_result
                    .content
                    .iter()
                    .map(|c| match &c.raw {
                        RawContent::Text(t) => t.text.clone(),
                        _ => serde_json::to_string(&c).unwrap_or_default(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };

            if call_result.is_error == Some(true) {
                return Err(McpError::Transport(format!(
                    "tool '{tool_name}' returned an error: {output}"
                )));
            }

            Ok(output)
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

    /// Read a resource from a specific MCP server by URI.
    ///
    /// Text resources are returned as-is. Blob resources are described with
    /// their URI and base64 length (the caller can further decode if needed).
    pub async fn read_resource(
        &self,
        server_name: &str,
        uri: &str,
    ) -> Result<String, McpError> {
        let session = self
            .connected
            .get(server_name)
            .ok_or_else(|| McpError::NotConnected(server_name.to_string()))?;

        let params = ReadResourceRequestParams::new(uri);
        let peer = session.peer.lock().await;
        let result = peer
            .read_resource(params)
            .await
            .map_err(|e| map_rmcp_error(format!("read_resource '{uri}': {e}"), &e.to_string()))?;

        let body = result
            .contents
            .into_iter()
            .map(|c| match c {
                ResourceContents::TextResourceContents { text, .. } => text,
                ResourceContents::BlobResourceContents { blob, uri, .. } => {
                    format!("[blob: uri={uri}, base64_len={}]", blob.len())
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(body)
    }

    /// Get connection statuses for all configured servers.
    pub fn list_statuses(&self) -> Vec<McpConnectionStatus> {
        self.configs
            .iter()
            .map(|(name, config)| {
                let transport = match config {
                    McpServerConfig::Stdio(_) => "stdio",
                    McpServerConfig::Http(_) => "http",
                    McpServerConfig::WebSocket(_) => "ws",
                };
                let auth_configured = cfg_has_auth(config);
                if let Some(session) = self.connected.get(name) {
                    McpConnectionStatus {
                        name: name.clone(),
                        state: session.state,
                        detail: String::new(),
                        transport: transport.into(),
                        auth_configured,
                        tools: session.tools.clone(),
                        resources: session.resources.clone(),
                    }
                } else {
                    McpConnectionStatus {
                        name: name.clone(),
                        state: McpConnectionState::Pending,
                        detail: "not connected".into(),
                        transport: transport.into(),
                        auth_configured,
                        tools: Vec::new(),
                        resources: Vec::new(),
                    }
                }
            })
            .collect()
    }

    /// Cancel all active sessions and clear connection state.
    pub async fn close(&mut self) {
        // Dropping each McpSession cancels its background task via `_cancel`.
        self.connected.clear();
    }

    /// Insert or replace a server configuration.
    pub fn update_server_config(&mut self, name: String, config: McpServerConfig) {
        self.configs.insert(name, config);
    }

    /// Ping a single connected MCP server to verify it is still alive.
    ///
    /// Uses `list_tools` as the liveness probe because rmcp does not yet expose
    /// a dedicated `ping` RPC in all protocol versions. On failure the session
    /// state is updated to `Disconnected`.
    pub async fn ping(&mut self, server: &str) -> Result<(), McpError> {
        let session = self
            .connected
            .get(server)
            .ok_or_else(|| McpError::NotConnected(server.to_string()))?;

        let peer = session.peer.lock().await;
        let result = peer
            .list_all_tools()
            .await
            .map_err(|e| map_rmcp_error(format!("ping '{server}': {e}"), &e.to_string()));

        // Drop the lock before mutating the session.
        drop(peer);

        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                // Mark the session as disconnected so callers see the updated state.
                if let Some(session) = self.connected.get_mut(server) {
                    session.state = McpConnectionState::Disconnected;
                }
                Err(e)
            }
        }
    }

    /// Ping all connected sessions and update their state.
    ///
    /// Returns a list of server names that failed the liveness check.
    pub async fn refresh_statuses(&mut self) -> Vec<String> {
        let names: Vec<String> = self.connected.keys().cloned().collect();
        let mut failed = Vec::new();
        for name in names {
            if let Err(_) = self.ping(&name).await {
                failed.push(name);
            }
        }
        failed
    }
}

/// Map an rmcp error string to the most specific `McpError` variant.
///
/// We inspect the display string because rmcp does not expose strongly-typed
/// error variants for every failure mode. In practice:
///   - "timed out" / "deadline"          → `Timeout`
///   - "unauthorized" / "forbidden" /
///     "authentication" / "401" / "403"  → `AuthFailed`
///   - "eof" / "broken pipe" /
///     "connection reset" / "disconnect" → `Disconnected`
///   - everything else                   → `Transport`
fn map_rmcp_error(msg: String, raw: &str) -> McpError {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") || lower.contains("deadline") {
        McpError::Timeout(msg)
    } else if lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("authentication")
        || lower.contains(" 401")
        || lower.contains(" 403")
    {
        McpError::AuthFailed(msg)
    } else if lower.contains("eof")
        || lower.contains("broken pipe")
        || lower.contains("connection reset")
        || lower.contains("disconnect")
        || lower.contains("channel closed")
    {
        McpError::Disconnected(msg)
    } else {
        McpError::Transport(msg)
    }
}

/// Returns `true` when a config has authentication headers configured.
fn cfg_has_auth(config: &McpServerConfig) -> bool {
    match config {
        McpServerConfig::Http(c) => c.headers.contains_key("Authorization"),
        McpServerConfig::WebSocket(c) => c.headers.contains_key("Authorization"),
        McpServerConfig::Stdio(_) => false,
    }
}

// ---------------------------------------------------------------------------
// MCP errors
// ---------------------------------------------------------------------------

/// Errors returned by `McpClientManager`.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("server not connected: {0}")]
    NotConnected(String),
    #[error("MCP feature not implemented: {0}")]
    NotImplemented(String),
    #[error("connection timed out: {0}")]
    Timeout(String),
    #[error("authentication failed: {0}")]
    AuthFailed(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("server disconnected: {0}")]
    Disconnected(String),
    #[error("tool not found: {server}:{tool}")]
    ToolNotFound { server: String, tool: String },
    #[error("transport error: {0}")]
    Transport(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use oh_types::mcp::{
        McpConnectionState, McpHttpServerConfig, McpServerConfig, McpStdioServerConfig,
        McpWebSocketServerConfig,
    };
    use std::collections::HashMap;

    // -----------------------------------------------------------------------
    // Pure unit tests (no network / process)
    // -----------------------------------------------------------------------

    #[test]
    fn test_new_manager_is_empty() {
        let mgr = McpClientManager::new(HashMap::new());
        assert!(mgr.list_tools().is_empty());
        assert!(mgr.list_resources().is_empty());
        assert!(mgr.list_statuses().is_empty());
    }

    #[test]
    fn test_list_statuses_pending_before_connect() {
        let mut configs = HashMap::new();
        configs.insert(
            "srv".into(),
            McpServerConfig::Stdio(McpStdioServerConfig {
                r#type: "stdio".into(),
                command: "cat".into(),
                args: vec![],
                env: None,
                cwd: None,
            }),
        );
        let mgr = McpClientManager::new(configs);
        let statuses = mgr.list_statuses();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].state, McpConnectionState::Pending);
        assert_eq!(statuses[0].transport, "stdio");
        assert_eq!(statuses[0].detail, "not connected");
    }

    #[test]
    fn test_update_server_config_adds_entry() {
        let mut mgr = McpClientManager::new(HashMap::new());
        mgr.update_server_config(
            "new_srv".into(),
            McpServerConfig::Http(McpHttpServerConfig {
                r#type: "http".into(),
                url: "http://localhost:9999/mcp".into(),
                headers: HashMap::new(),
            }),
        );
        let statuses = mgr.list_statuses();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].transport, "http");
        assert_eq!(statuses[0].state, McpConnectionState::Pending);
    }

    #[test]
    fn test_websocket_config_transport_label() {
        let mut configs = HashMap::new();
        configs.insert(
            "wssrv".into(),
            McpServerConfig::WebSocket(McpWebSocketServerConfig {
                r#type: "ws".into(),
                url: "ws://localhost:1234".into(),
                headers: HashMap::new(),
            }),
        );
        let mgr = McpClientManager::new(configs);
        let statuses = mgr.list_statuses();
        assert_eq!(statuses[0].transport, "ws");
        assert_eq!(statuses[0].state, McpConnectionState::Pending);
    }

    #[test]
    fn test_cfg_has_auth_http_with_auth_header() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".into(), "Bearer token123".into());
        let config = McpServerConfig::Http(McpHttpServerConfig {
            r#type: "http".into(),
            url: "http://example.com/mcp".into(),
            headers,
        });
        assert!(cfg_has_auth(&config));
    }

    #[test]
    fn test_cfg_has_auth_http_without_auth_header() {
        let config = McpServerConfig::Http(McpHttpServerConfig {
            r#type: "http".into(),
            url: "http://example.com/mcp".into(),
            headers: HashMap::new(),
        });
        assert!(!cfg_has_auth(&config));
    }

    #[test]
    fn test_cfg_has_auth_stdio_always_false() {
        let config = McpServerConfig::Stdio(McpStdioServerConfig {
            r#type: "stdio".into(),
            command: "node".into(),
            args: vec![],
            env: None,
            cwd: None,
        });
        assert!(!cfg_has_auth(&config));
    }

    // -----------------------------------------------------------------------
    // Async unit tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_call_tool_returns_not_connected() {
        let mgr = McpClientManager::new(HashMap::new());
        let err = mgr
            .call_tool("ghost", "something", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, McpError::NotConnected(_)));
        assert!(err.to_string().contains("ghost"));
    }

    #[tokio::test]
    async fn test_read_resource_returns_not_connected() {
        let mgr = McpClientManager::new(HashMap::new());
        let err = mgr
            .read_resource("ghost", "file:///foo")
            .await
            .unwrap_err();
        assert!(matches!(err, McpError::NotConnected(_)));
    }

    #[tokio::test]
    async fn test_call_tool_array_args_not_connected_first() {
        // The NotConnected check fires before argument validation.
        let mgr = McpClientManager::new(HashMap::new());
        let err = mgr
            .call_tool("ghost", "tool", serde_json::json!([1, 2, 3]))
            .await
            .unwrap_err();
        assert!(matches!(err, McpError::NotConnected(_)));
    }

    #[tokio::test]
    async fn test_connect_all_stdio_nonexistent_binary_fails() {
        let mut configs = HashMap::new();
        configs.insert(
            "bad".into(),
            McpServerConfig::Stdio(McpStdioServerConfig {
                r#type: "stdio".into(),
                command: "__oh_nonexistent_binary_12345__".into(),
                args: vec![],
                env: None,
                cwd: None,
            }),
        );
        let mut mgr = McpClientManager::new(configs);
        let statuses = mgr.connect_all().await;
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].state, McpConnectionState::Failed);
        assert!(!statuses[0].detail.is_empty(), "detail should contain the error");
    }

    #[tokio::test]
    async fn test_connect_all_websocket_returns_failed() {
        // WebSocket is not implemented; connect_all should mark it Failed.
        let mut configs = HashMap::new();
        configs.insert(
            "wssrv".into(),
            McpServerConfig::WebSocket(McpWebSocketServerConfig {
                r#type: "ws".into(),
                url: "ws://localhost:9999".into(),
                headers: HashMap::new(),
            }),
        );
        let mut mgr = McpClientManager::new(configs);
        let statuses = mgr.connect_all().await;
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].state, McpConnectionState::Failed);
    }

    #[tokio::test]
    async fn test_close_clears_sessions() {
        let mut mgr = McpClientManager::new(HashMap::new());
        // Even with no sessions, close() should not panic.
        mgr.close().await;
        assert!(mgr.list_tools().is_empty());
    }

    #[tokio::test]
    async fn test_error_display_messages() {
        let e1 = McpError::NotConnected("mysrv".into());
        assert!(e1.to_string().contains("mysrv"));
        let e2 = McpError::NotImplemented("ws not supported".into());
        assert!(e2.to_string().contains("not implemented"));
        let e3 = McpError::Transport("some transport failure".into());
        assert!(e3.to_string().contains("transport error"));
    }

    #[test]
    fn test_mcp_error_new_variants_display() {
        let e_timeout = McpError::Timeout("connect timed out after 30s".into());
        assert!(e_timeout.to_string().contains("timed out"));

        let e_auth = McpError::AuthFailed("401 unauthorized".into());
        assert!(e_auth.to_string().contains("authentication failed"));

        let e_proto = McpError::Protocol("unexpected message type".into());
        assert!(e_proto.to_string().contains("protocol error"));

        let e_disc = McpError::Disconnected("EOF on stdin".into());
        assert!(e_disc.to_string().contains("disconnected"));

        let e_tool = McpError::ToolNotFound {
            server: "my-server".into(),
            tool: "do_thing".into(),
        };
        assert!(e_tool.to_string().contains("my-server"));
        assert!(e_tool.to_string().contains("do_thing"));
    }

    #[test]
    fn test_map_rmcp_error_routing() {
        // Timeout keywords
        let e = map_rmcp_error("msg".into(), "operation timed out after 5s");
        assert!(matches!(e, McpError::Timeout(_)));

        // Auth keywords
        let e = map_rmcp_error("msg".into(), "server returned 401 unauthorized");
        assert!(matches!(e, McpError::AuthFailed(_)));

        // Disconnect keywords
        let e = map_rmcp_error("msg".into(), "broken pipe");
        assert!(matches!(e, McpError::Disconnected(_)));

        // Fallback
        let e = map_rmcp_error("msg".into(), "something completely different");
        assert!(matches!(e, McpError::Transport(_)));
    }

    #[tokio::test]
    async fn test_ping_not_connected_server_returns_error() {
        let mgr = McpClientManager::new(HashMap::new());
        // Wrap in Mutex so we can call &mut self
        let mut mgr = mgr;
        let err = mgr.ping("ghost").await.unwrap_err();
        assert!(matches!(err, McpError::NotConnected(_)));
        assert!(err.to_string().contains("ghost"));
    }

    #[tokio::test]
    async fn test_refresh_statuses_empty_manager() {
        let mut mgr = McpClientManager::new(HashMap::new());
        let failed = mgr.refresh_statuses().await;
        assert!(failed.is_empty());
    }
}
