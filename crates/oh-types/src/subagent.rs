//! Subagent orchestration types and service trait objects.
//!
//! These trait objects let tools reach orchestration services (the
//! `SubagentManager` / `BackgroundTaskManager` that live in `oh-services`)
//! without `oh-types` ever depending on `oh-services` — exactly the pattern
//! used for [`StreamingApiClient`](crate::api) and `HookExecutorTrait`: the
//! abstraction is declared low in the dependency graph and implemented higher
//! up. See the subagent orchestration design spec.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::tasks::{TaskRecord, TaskStatus};

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Opaque identifier for an agent run (the top-level agent or a subagent).
///
/// Mirrors the `TeammateId` / `TeamId` newtype style from `oh-swarm`, but lives
/// in `oh-types` because both the engine (`QueryContext`) and services need it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub String);

impl AgentId {
    pub fn new(s: impl Into<String>) -> Self {
        AgentId(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors surfaced when spawning a subagent.
#[derive(Debug, thiserror::Error)]
pub enum SubagentError {
    #[error("backend not yet implemented in this phase: {0}")]
    BackendUnimplemented(String),

    #[error("unknown subagent type: {0}")]
    UnknownSubagentType(String),

    #[error("spawn failed: {0}")]
    Spawn(String),
}

// ---------------------------------------------------------------------------
// Spawn request / result value types
// ---------------------------------------------------------------------------

/// How a subagent should be isolated from its parent.
///
/// Kept as a small standalone enum (rather than re-using the richer
/// `IsolationMode` in `oh-services`) so it stays in the root crate without a
/// dependency edge; `oh-services` maps between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SubagentIsolation {
    /// Run as a tokio task in the current process (default).
    #[default]
    InProcess,
    /// Run as a separate `oh` subprocess.
    Subprocess,
    /// Run as a subprocess inside a fresh git worktree.
    Worktree,
}

/// Parameters for spawning a subagent.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// Stable id assigned to the spawned agent (caller-chosen or generated).
    pub agent_id: AgentId,
    /// Routing key resolved to an `AgentDefinition` (e.g. `general-purpose`).
    pub subagent_type: String,
    /// The seed prompt for the subagent.
    pub prompt: String,
    /// Optional model override.
    pub model: Option<String>,
    /// Whether the spawn should run in the background (return a handle now).
    pub run_in_background: bool,
    /// Isolation mode for the backend selection.
    pub isolation: SubagentIsolation,
}

impl SpawnRequest {
    /// Build a minimal in-process, backgrounded request for `subagent_type`.
    pub fn new(
        agent_id: AgentId,
        subagent_type: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        Self {
            agent_id,
            subagent_type: subagent_type.into(),
            prompt: prompt.into(),
            model: None,
            run_in_background: true,
            isolation: SubagentIsolation::default(),
        }
    }
}

/// Handle returned from a successful spawn.
///
/// Spec value type: `{ agent_id, task_id }`. The `SubagentSpawner::spawn`
/// fallibility is expressed by wrapping this in a `Result<_, SubagentError>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnResult {
    pub agent_id: AgentId,
    /// The `TaskRecord` id under which this spawn is tracked.
    pub task_id: String,
}

// ---------------------------------------------------------------------------
// Service trait objects (implemented in oh-services)
// ---------------------------------------------------------------------------

/// Spawns subagents. Implemented by `oh_services::subagent::SubagentManager`.
///
/// Tools reach this via `ToolExecutionContext::subagents`.
#[async_trait]
pub trait SubagentSpawner: Send + Sync {
    async fn spawn(&self, req: SpawnRequest) -> Result<SpawnResult, SubagentError>;
}

/// Runs a single in-process subagent to completion and returns its final
/// assistant text.
///
/// This is the seam that breaks the would-be dependency cycle. Building a child
/// `QueryContext` requires `oh-engine`/`oh-tools`/`oh-api` types, but
/// `oh-services` (where `SubagentManager` lives) is *below* `oh-engine` in the
/// dependency graph (`oh-engine → oh-tools → oh-services`), so it cannot call
/// `oh_engine::run_subagent` directly. Instead the harness — which sits on top
/// of everything — constructs a `SubagentRunner` that owns the
/// `QueryContext`-building logic and injects it into the `SubagentManager`. The
/// manager invokes it from inside the background task it records, keeping the
/// orchestration (definition resolution, backend selection, task recording) in
/// `oh-services` while the engine wiring stays in `oh-harness`.
#[async_trait]
pub trait SubagentRunner: Send + Sync {
    /// Run the subagent described by `req` (already resolved to `subagent_type`)
    /// to completion. `tools` is the effective allowed-tool name set after
    /// intersecting the agent definition's policy with the parent's tools; an
    /// empty set means "no restriction beyond the parent's registry".
    async fn run(&self, req: SpawnRequest, allowed_tools: Vec<String>) -> Result<String, String>;
}

/// Background-task control plane over `TaskRecord`s. Implemented by
/// `oh_services::tasks::BackgroundTaskManager`.
///
/// Method signatures mirror `BackgroundTaskManager`'s inherent methods so the
/// impl is a thin delegation. Tools reach this via
/// `ToolExecutionContext::tasks`.
#[async_trait]
pub trait BackgroundTasks: Send + Sync {
    async fn create_shell(&self, command: &str, description: &str, cwd: &str) -> TaskRecord;
    async fn create_agent(&self, prompt: &str, description: &str, cwd: &str) -> TaskRecord;
    async fn get(&self, id: &str) -> Option<TaskRecord>;
    async fn list(&self, status: Option<TaskStatus>) -> Vec<TaskRecord>;
    async fn stop(&self, id: &str) -> Option<TaskRecord>;
    async fn read_output(&self, id: &str, max_bytes: usize) -> Result<String, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_id_display_and_accessor() {
        let id = AgentId::new("main");
        assert_eq!(id.as_str(), "main");
        assert_eq!(format!("{id}"), "main");
    }

    #[test]
    fn test_agent_id_serde_roundtrip() {
        let id = AgentId::new("sub-1");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"sub-1\"");
        let deser: AgentId = serde_json::from_str(&json).unwrap();
        assert_eq!(deser, id);
    }

    #[test]
    fn test_subagent_isolation_default_is_in_process() {
        assert_eq!(SubagentIsolation::default(), SubagentIsolation::InProcess);
    }

    #[test]
    fn test_subagent_isolation_serde_values() {
        assert_eq!(
            serde_json::to_string(&SubagentIsolation::InProcess).unwrap(),
            "\"in_process\""
        );
        assert_eq!(
            serde_json::to_string(&SubagentIsolation::Worktree).unwrap(),
            "\"worktree\""
        );
    }

    #[test]
    fn test_spawn_request_new_defaults() {
        let req = SpawnRequest::new(AgentId::new("a1"), "general-purpose", "do it");
        assert_eq!(req.agent_id, AgentId::new("a1"));
        assert_eq!(req.subagent_type, "general-purpose");
        assert_eq!(req.prompt, "do it");
        assert!(req.model.is_none());
        assert!(req.run_in_background);
        assert_eq!(req.isolation, SubagentIsolation::InProcess);
    }

    #[test]
    fn test_spawn_result_serde_roundtrip() {
        let res = SpawnResult {
            agent_id: AgentId::new("a1"),
            task_id: "task-7".into(),
        };
        let json = serde_json::to_string(&res).unwrap();
        let deser: SpawnResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deser, res);
    }

    #[test]
    fn test_subagent_runner_is_object_safe() {
        // `SubagentRunner` must be usable as a trait object (the manager holds an
        // `Arc<dyn SubagentRunner>`). Constructing one here proves object safety
        // without needing an async runtime in this dependency-free crate.
        struct EchoRunner;
        #[async_trait]
        impl SubagentRunner for EchoRunner {
            async fn run(
                &self,
                req: SpawnRequest,
                allowed_tools: Vec<String>,
            ) -> Result<String, String> {
                Ok(format!("{}|tools={}", req.prompt, allowed_tools.len()))
            }
        }
        let _runner: Box<dyn SubagentRunner> = Box::new(EchoRunner);
    }

    #[test]
    fn test_subagent_error_display() {
        let err = SubagentError::BackendUnimplemented("subprocess".into());
        assert_eq!(
            format!("{err}"),
            "backend not yet implemented in this phase: subprocess"
        );
    }
}
