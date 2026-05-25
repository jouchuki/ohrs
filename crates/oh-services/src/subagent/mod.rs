//! Subagent orchestration: backend selection, the spawn manager, and the
//! trajectory recorder.
//!
//! Phase 0 wires the seams (trait impls, registry, built-in agent defs,
//! recorder) so Phases 1-3 can fill in the real spawn paths. See the subagent
//! orchestration design spec.

pub mod backend_registry;
pub mod manager;
pub mod trajectory;

pub use backend_registry::{BackendRegistry, TEAMMATE_MODE_ENV};
pub use manager::SubagentManager;
pub use trajectory::TrajectoryRecorder;
