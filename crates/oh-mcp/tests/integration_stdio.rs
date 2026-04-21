//! Integration tests for the stdio MCP transport using the Python echo-server fixture.
//!
//! The fixture at `tests/fixtures/echo_server.py` is a real subprocess that speaks
//! JSON-RPC 2.0 over stdin/stdout. These tests verify a genuine end-to-end round-trip.

use oh_mcp::client::{McpClientManager, McpError};
use oh_types::mcp::{McpConnectionState, McpServerConfig, McpStdioServerConfig};
use std::collections::HashMap;

/// Absolute path to the echo server fixture, computed relative to this file at
/// compile time so it works regardless of the working directory cargo runs in.
fn fixture_path() -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    format!("{manifest_dir}/tests/fixtures/echo_server.py")
}

fn echo_server_config() -> McpServerConfig {
    McpServerConfig::Stdio(McpStdioServerConfig {
        r#type: "stdio".into(),
        command: "python3".into(),
        args: vec![fixture_path()],
        env: None,
        cwd: None,
    })
}

/// Helper: build a manager pointing at the echo fixture and connect it.
async fn connected_echo_manager() -> McpClientManager {
    let mut configs = HashMap::new();
    configs.insert("echo".into(), echo_server_config());
    let mut mgr = McpClientManager::new(configs);
    let statuses = mgr.connect_all().await;
    assert_eq!(statuses.len(), 1, "expected exactly one status");
    assert_eq!(
        statuses[0].state,
        McpConnectionState::Connected,
        "echo server should connect successfully; detail={}",
        statuses[0].detail
    );
    mgr
}

// ---------------------------------------------------------------------------
// Test 1 — basic connection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_stdio_connect_state_is_connected() {
    // This test exercises connect_all() against a real subprocess.
    let mgr = connected_echo_manager().await;
    let statuses = mgr.list_statuses();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].state, McpConnectionState::Connected);
    assert_eq!(statuses[0].transport, "stdio");
}

// ---------------------------------------------------------------------------
// Test 2 — list_tools returns the echo tool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_stdio_list_tools_contains_echo() {
    let mgr = connected_echo_manager().await;
    let tools = mgr.list_tools();
    assert!(
        tools.iter().any(|t| t.name == "echo"),
        "expected 'echo' in tool list; got: {:?}",
        tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Test 3 — call_tool("echo") returns the input message
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_stdio_call_tool_echo_returns_message() {
    let mgr = connected_echo_manager().await;
    let result = mgr
        .call_tool("echo", "echo", serde_json::json!({"msg": "hi"}))
        .await
        .expect("call_tool should succeed");
    assert!(
        result.contains("hi"),
        "expected 'hi' in response; got: {result:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 4 — call_tool with unknown tool returns ToolNotFound
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_stdio_call_unknown_tool_returns_tool_not_found() {
    let mgr = connected_echo_manager().await;
    let err = mgr
        .call_tool("echo", "no_such_tool", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, McpError::ToolNotFound { .. }),
        "expected ToolNotFound, got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 5 — ping live server succeeds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_stdio_ping_live_server_succeeds() {
    let mut mgr = connected_echo_manager().await;
    mgr.ping("echo")
        .await
        .expect("ping against live echo server should succeed");
    // State should still be Connected after a successful ping.
    let statuses = mgr.list_statuses();
    assert_eq!(statuses[0].state, McpConnectionState::Connected);
}

// ---------------------------------------------------------------------------
// Test 6 — refresh_statuses returns no failures for a live server
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_stdio_refresh_statuses_no_failures() {
    let mut mgr = connected_echo_manager().await;
    let failed = mgr.refresh_statuses().await;
    assert!(
        failed.is_empty(),
        "expected no failures for live echo server; got: {failed:?}"
    );
}
