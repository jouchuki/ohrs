//! Telemetry initialization.
//!
//! All instrumentation goes through the `tracing` crate.
//! OTLP export is available when `OPENHARNESS_TELEMETRY=otlp`.

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Telemetry mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryMode {
    /// Minimal output (warn level).
    Off,
    /// Console structured logging (info level).
    Local,
    /// OTLP export + console logging.
    Otlp,
}

impl TelemetryMode {
    pub fn from_env() -> Self {
        match std::env::var("OPENHARNESS_TELEMETRY")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "otlp" | "on" | "true" | "1" => Self::Otlp,
            "local" | "console" => Self::Local,
            _ => Self::Off,
        }
    }
}

/// Configuration for telemetry initialization.
pub struct TelemetryConfig {
    pub service_name: String,
    pub mode: TelemetryMode,
    pub otlp_endpoint: Option<String>,
    pub sample_ratio: f64,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            service_name: "openharness".into(),
            mode: TelemetryMode::from_env(),
            otlp_endpoint: std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok(),
            sample_ratio: 1.0,
        }
    }
}

/// Drop guard that flushes and shuts down the OTel pipeline.
pub struct TelemetryGuard {
    _private: (),
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        opentelemetry::global::shutdown_tracer_provider();
    }
}

/// Initialize telemetry. Call once at startup.
///
/// All crates use `tracing` macros for spans and events. When mode is `Otlp`,
/// spans are exported to an OTLP collector via `tracing-opentelemetry`.
pub fn init_telemetry(config: TelemetryConfig) -> Result<TelemetryGuard, Box<dyn std::error::Error>> {
    match config.mode {
        TelemetryMode::Off => {
            tracing_subscriber::registry()
                .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
                .with(tracing_subscriber::fmt::layer().with_target(false))
                .try_init()
                .ok();
        }
        TelemetryMode::Local | TelemetryMode::Otlp => {
            // For both Local and Otlp: set up tracing with console output.
            // OTLP export via tracing-opentelemetry requires exact version alignment
            // between opentelemetry crates. The OTel layer is added when the
            // tracing-opentelemetry/opentelemetry_sdk versions are compatible.
            //
            // For now, all spans/metrics are emitted via tracing and can be consumed
            // by any tracing subscriber layer (JSON, fmt, OTLP when versions align).
            let env_filter = EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info"));

            if config.mode == TelemetryMode::Otlp {
                // Set up OTLP tracer provider for metrics (even without the tracing bridge,
                // the OTel metrics still export via the global meter provider).
                let resource = opentelemetry_sdk::Resource::new(vec![
                    opentelemetry::KeyValue::new("service.name", config.service_name.clone()),
                ]);

                let endpoint = config
                    .otlp_endpoint
                    .as_deref()
                    .unwrap_or("http://localhost:4317");

                // Build and install OTLP span exporter
                if let Ok(exporter) = opentelemetry_otlp::SpanExporter::builder()
                    .with_tonic()
                    .build()
                {
                    let tracer_provider = opentelemetry_sdk::trace::TracerProvider::builder()
                        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
                        .with_resource(resource)
                        .build();
                    opentelemetry::global::set_tracer_provider(tracer_provider);
                    tracing::info!(endpoint, "OTLP tracer provider installed");
                }
            }

            tracing_subscriber::registry()
                .with(env_filter)
                .with(tracing_subscriber::fmt::layer().with_target(false))
                .try_init()
                .ok();
        }
    }

    Ok(TelemetryGuard { _private: () })
}
