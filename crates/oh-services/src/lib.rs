//! Supporting services for OpenHarness.

pub mod bridge;
pub mod compact;
pub mod coordinator;
pub mod cron;
pub mod memory;
pub mod prompts;
pub mod sessions;
pub mod skills;
pub mod subagent;
pub mod tasks;
pub mod token_estimation;

pub use bridge::{
    generate_work_secret, validate_work_secret, BridgeError, BridgeManager, BridgeSessionState,
};
