//! `SubagentManager` — resolves an agent type, selects a backend, and records
//! a uniform [`TaskRecord`] for every spawn.
//!
//! Implements [`oh_types::subagent::SubagentSpawner`] so tools reach it through
//! the trait object on `ToolExecutionContext` — no `oh-types → oh-services`
//! edge.
//!
//! Phase 0 scope: resolve `subagent_type` to an [`AgentDefinition`], select the
//! backend via [`BackendRegistry`] (which fails for not-yet-implemented
//! Subprocess/Worktree modes), and register a `TaskRecord` through the
//! [`BackgroundTaskManager`]. Driving the backend to actually run
//! `run_subagent` is Phase 1.

use std::sync::Arc;

use async_trait::async_trait;
use oh_types::subagent::{SpawnRequest, SpawnResult, SubagentError, SubagentSpawner};

use crate::coordinator::agent_definitions::{self, AgentDefinition};
use crate::subagent::backend_registry::BackendRegistry;
use crate::tasks::BackgroundTaskManager;

/// Orchestrates subagent spawns.
pub struct SubagentManager {
    tasks: Arc<BackgroundTaskManager>,
    backends: BackendRegistry,
    /// Working directory recorded on spawned task records.
    cwd: String,
}

impl SubagentManager {
    /// Create a manager that records tasks via `tasks` and runs spawns in
    /// `cwd`.
    pub fn new(tasks: Arc<BackgroundTaskManager>, cwd: impl Into<String>) -> Self {
        Self {
            tasks,
            backends: BackendRegistry::new(),
            cwd: cwd.into(),
        }
    }

    /// Resolve a `subagent_type` to its [`AgentDefinition`] (built-ins for now;
    /// the YAML overlay is applied by the harness when it has a project root).
    pub fn resolve_definition(&self, subagent_type: &str) -> AgentDefinition {
        agent_definitions::resolve(subagent_type)
    }
}

#[async_trait]
impl SubagentSpawner for SubagentManager {
    async fn spawn(&self, req: SpawnRequest) -> Result<SpawnResult, SubagentError> {
        // Resolve the agent definition (validates the type via fallback).
        let def = self.resolve_definition(&req.subagent_type);

        // Select the backend. Phase 0: only InProcess resolves; Subprocess /
        // Worktree surface BackendUnimplemented here.
        let _backend = self.backends.select(req.isolation)?;

        // Record a uniform TaskRecord for the spawn so every backend exposes the
        // same handle. Phase 1 replaces the echo stub with the real run.
        let description = format!("subagent {} ({})", req.agent_id, def.name);
        let record = self
            .tasks
            .create_agent_task(&req.prompt, &description, &self.cwd)
            .await;

        Ok(SpawnResult {
            agent_id: req.agent_id,
            task_id: record.id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oh_types::subagent::{AgentId, SubagentIsolation};

    fn manager() -> SubagentManager {
        SubagentManager::new(Arc::new(BackgroundTaskManager::new()), "/tmp")
    }

    #[tokio::test]
    async fn test_spawn_in_process_returns_handle() {
        let mgr = manager();
        let req = SpawnRequest::new(AgentId::new("sub-1"), "general-purpose", "hello");
        let res = mgr.spawn(req).await.unwrap();
        assert_eq!(res.agent_id, AgentId::new("sub-1"));
        assert!(!res.task_id.is_empty());
        // The task is registered with the manager.
        assert!(mgr.tasks.get_task(&res.task_id).await.is_some());
    }

    #[tokio::test]
    async fn test_spawn_subprocess_unimplemented() {
        let mgr = manager();
        let mut req = SpawnRequest::new(AgentId::new("sub-2"), "worker", "go");
        req.isolation = SubagentIsolation::Subprocess;
        let err = mgr.spawn(req).await.unwrap_err();
        assert!(matches!(err, SubagentError::BackendUnimplemented(_)));
    }

    #[test]
    fn test_resolve_definition_uses_builtins() {
        let mgr = manager();
        assert_eq!(mgr.resolve_definition("Explore").name, "Explore");
        assert_eq!(
            mgr.resolve_definition("unknown-type").name,
            "general-purpose"
        );
    }
}
