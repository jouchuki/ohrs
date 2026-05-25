//! Configuration, settings, and path resolution for OpenHarness.

pub mod paths;
pub mod settings;

pub use paths::*;
pub use settings::*;

/// Serializes tests that mutate process-global environment variables.
///
/// Unit tests across this crate compile into a single test binary and run in
/// parallel by default. Several tests in `settings.rs` and `paths.rs` mutate
/// shared `ANTHROPIC_*` / `OPENHARNESSRS_*` env vars, so they must acquire this
/// lock before touching the environment to avoid clobbering each other.
#[cfg(test)]
pub(crate) static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
