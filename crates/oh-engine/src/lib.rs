//! Query engine: conversation loop, tool dispatch, cost tracking.

pub mod cost_tracker;
pub mod query;
pub mod query_engine;

pub use query::*;
pub use query_engine::QueryEngine;

/// Re-export so downstream crates (the harness) can construct/clone the engine's
/// cancellation token without taking a direct `tokio-util` dependency (ENG-2).
pub use tokio_util::sync::CancellationToken;
