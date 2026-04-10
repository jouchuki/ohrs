//! OpenHarness CLI — an AI-powered coding assistant.

mod cli;
mod ui;

use clap::Parser;

#[tokio::main]
async fn main() {
    let args = cli::Args::parse();

    // Initialize telemetry
    let _guard = oh_telemetry::init_telemetry(oh_telemetry::TelemetryConfig::default())
        .expect("failed to initialize telemetry");

    if let Err(e) = cli::run(args).await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
