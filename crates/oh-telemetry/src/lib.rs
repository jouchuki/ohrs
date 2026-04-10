//! OpenTelemetry initialization, span helpers, and metrics for OpenHarness.
//!
//! Call [`init_telemetry`] once at startup. Library crates use `tracing` macros;
//! spans are exported to OTLP when configured.

pub mod init;
pub mod metrics;

pub use init::{init_telemetry, TelemetryConfig, TelemetryGuard};
pub use metrics::*;
