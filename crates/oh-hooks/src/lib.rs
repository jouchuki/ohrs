//! Hook system with 21 lifecycle events and 4 executor types.
//!
//! Hook types: command, http, prompt, agent.
//! All 21 [`HookEvent`] variants are hookable.

pub mod executor;
pub mod loader;
pub mod matching;

pub use oh_types::hooks::{
    AggregatedHookResult, HookDefinition, HookEvent, HookResult,
};

use async_trait::async_trait;

/// Trait for hook execution — used by the engine.
#[async_trait]
pub trait HookExecutorTrait: Send + Sync {
    async fn execute(
        &self,
        event: HookEvent,
        payload: serde_json::Value,
    ) -> AggregatedHookResult;
}
