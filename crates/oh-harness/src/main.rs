//! ohrs — an AI-powered coding assistant.

mod cli;
mod run_once;
mod subagent_runner;
mod trajectory;
mod ui;

use clap::Parser;

#[tokio::main]
async fn main() {
    let args = cli::Args::parse();

    // Initialize telemetry
    let _guard = oh_telemetry::init_telemetry(oh_telemetry::TelemetryConfig::default())
        .expect("failed to initialize telemetry");

    // Install a SIGTERM/SIGINT handler that finalizes the active trajectory
    // (contract C1: the `end` line must be written even on signal-driven
    // teardown). The handler finalizes the process-wide writer, then exits so
    // the Drop guard cannot race a partially-flushed file.
    install_signal_finalizer();

    if let Err(e) = cli::run(args).await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

/// Spawn a task that, on SIGTERM/SIGINT, finalizes any active trajectory with an
/// `error` status and exits. No-op on platforms without unix signals.
#[cfg(unix)]
fn install_signal_finalizer() {
    use tokio::signal::unix::{signal, SignalKind};
    tokio::spawn(async move {
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "could not install SIGTERM handler");
                return;
            }
        };
        let mut int = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "could not install SIGINT handler");
                return;
            }
        };
        let reason = tokio::select! {
            _ = term.recv() => "SIGTERM",
            _ = int.recv() => "SIGINT",
        };
        tracing::warn!(signal = reason, "received termination signal; finalizing trajectory");
        cli::finalize_active_trajectory(reason);
        std::process::exit(130);
    });
}

#[cfg(not(unix))]
fn install_signal_finalizer() {}
