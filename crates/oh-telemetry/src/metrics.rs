//! Pre-defined OTel metrics for OpenHarness.

use opentelemetry::global;
use opentelemetry::metrics::{Counter, Histogram, UpDownCounter};
use std::sync::LazyLock;

/// Duration of tool calls in seconds.
pub static TOOL_CALL_DURATION: LazyLock<Histogram<f64>> = LazyLock::new(|| {
    global::meter("openharness")
        .f64_histogram("tool_call_duration_seconds")
        .with_description("Duration of tool calls")
        .with_unit("s")
        .build()
});

/// Number of tool errors.
pub static TOOL_ERROR_COUNT: LazyLock<Counter<u64>> = LazyLock::new(|| {
    global::meter("openharness")
        .u64_counter("tool_error_total")
        .with_description("Total tool execution errors")
        .build()
});

/// Duration of API requests in seconds.
pub static API_REQUEST_DURATION: LazyLock<Histogram<f64>> = LazyLock::new(|| {
    global::meter("openharness")
        .f64_histogram("api_request_duration_seconds")
        .with_description("Duration of API requests to model providers")
        .with_unit("s")
        .build()
});

/// Total tokens consumed.
pub static TOKEN_USAGE_TOTAL: LazyLock<Counter<u64>> = LazyLock::new(|| {
    global::meter("openharness")
        .u64_counter("token_usage_total")
        .with_description("Total tokens consumed")
        .build()
});

/// Duration of hook executions in seconds.
pub static HOOK_EXECUTION_DURATION: LazyLock<Histogram<f64>> = LazyLock::new(|| {
    global::meter("openharness")
        .f64_histogram("hook_execution_duration_seconds")
        .with_description("Duration of hook executions")
        .with_unit("s")
        .build()
});

/// Number of hooks that blocked an operation.
pub static HOOK_BLOCKED_COUNT: LazyLock<Counter<u64>> = LazyLock::new(|| {
    global::meter("openharness")
        .u64_counter("hook_blocked_total")
        .with_description("Total hook blocks")
        .build()
});

/// Total permission checks performed.
pub static PERMISSION_CHECK_TOTAL: LazyLock<Counter<u64>> = LazyLock::new(|| {
    global::meter("openharness")
        .u64_counter("permission_check_total")
        .with_description("Total permission checks performed")
        .build()
});

/// Permission denials.
pub static PERMISSION_DENIED_COUNT: LazyLock<Counter<u64>> = LazyLock::new(|| {
    global::meter("openharness")
        .u64_counter("permission_denied_total")
        .with_description("Total permission denials")
        .build()
});

/// Plugin load duration.
pub static PLUGIN_LOAD_DURATION: LazyLock<Histogram<f64>> = LazyLock::new(|| {
    global::meter("openharness")
        .f64_histogram("plugin_load_duration_seconds")
        .with_description("Duration of plugin loading")
        .with_unit("s")
        .build()
});

/// MCP tool call duration.
pub static MCP_CALL_DURATION: LazyLock<Histogram<f64>> = LazyLock::new(|| {
    global::meter("openharness")
        .f64_histogram("mcp_call_duration_seconds")
        .with_description("Duration of MCP tool calls")
        .with_unit("s")
        .build()
});

/// Number of active sessions.
pub static ACTIVE_SESSIONS: LazyLock<UpDownCounter<i64>> = LazyLock::new(|| {
    global::meter("openharness")
        .i64_up_down_counter("active_sessions")
        .with_description("Number of active sessions")
        .build()
});

/// Number of active background tasks.
pub static ACTIVE_BACKGROUND_TASKS: LazyLock<UpDownCounter<i64>> = LazyLock::new(|| {
    global::meter("openharness")
        .i64_up_down_counter("active_background_tasks")
        .with_description("Number of active background tasks")
        .build()
});

/// Session duration.
pub static SESSION_DURATION: LazyLock<Histogram<f64>> = LazyLock::new(|| {
    global::meter("openharness")
        .f64_histogram("session_duration_seconds")
        .with_description("Duration of sessions")
        .with_unit("s")
        .build()
});
