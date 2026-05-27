//! `SubagentManager` — resolves an agent type, selects a backend, and records
//! a uniform [`TaskRecord`] for every spawn.
//!
//! Implements [`oh_types::subagent::SubagentSpawner`] so tools reach it through
//! the trait object on `ToolExecutionContext` — no `oh-types → oh-services`
//! edge.
//!
//! ## Dependency direction
//!
//! Building a child `QueryContext` requires `oh-engine`/`oh-tools`/`oh-api`
//! types, but `oh-services` sits *below* `oh-engine` in the dependency graph
//! (`oh-engine → oh-tools → oh-services`), so this crate cannot call
//! `oh_engine::run_subagent` directly without a cycle. Instead the manager holds
//! an injected [`oh_types::subagent::SubagentRunner`] — built by `oh-harness`,
//! which sits on top of everything — that owns the `QueryContext` construction.
//! The manager keeps the orchestration here (definition resolution, tool-policy
//! intersection, backend selection, task recording) and delegates only the
//! engine-bound run to the injected runner. When no runner is injected (e.g.
//! plain unit tests), the in-process spawn echoes the prompt back so the task
//! infrastructure is still exercised.

use std::sync::Arc;

use async_trait::async_trait;
use oh_swarm::{Mailbox, Message, MessageKind, TeammateId};
use oh_types::subagent::{
    SpawnRequest, SpawnResult, SubagentError, SubagentIsolation, SubagentRunner, SubagentSpawner,
};

use crate::coordinator::agent_definitions::{self, AgentDefinition, ToolPolicy};
use crate::subagent::backend_registry::BackendRegistry;
use crate::tasks::BackgroundTaskManager;

/// Orchestrates subagent spawns.
pub struct SubagentManager {
    tasks: Arc<BackgroundTaskManager>,
    backends: BackendRegistry,
    /// Working directory recorded on spawned task records.
    cwd: String,
    /// Optional engine-bound runner (injected by the harness). When present,
    /// the in-process spawn path drives a real nested `run_subagent`.
    runner: Option<Arc<dyn SubagentRunner>>,
    /// The full set of tool names available to the parent. Used to resolve a
    /// `DenyList`/`AllowAll` policy into a concrete allow-list intersected with
    /// the parent's tools. Empty means "no universe known" → no restriction.
    tool_universe: Vec<String>,
    /// Root under which per-agent mailboxes live (mirrors `InProcessBackend`).
    team_root: std::path::PathBuf,
}

impl SubagentManager {
    /// Create a manager that records tasks via `tasks` and runs spawns in
    /// `cwd`. No engine runner is injected; the in-process path echoes.
    pub fn new(tasks: Arc<BackgroundTaskManager>, cwd: impl Into<String>) -> Self {
        Self {
            tasks,
            backends: BackendRegistry::new(),
            cwd: cwd.into(),
            runner: None,
            tool_universe: Vec::new(),
            team_root: oh_config::get_tasks_dir().join("teammates"),
        }
    }

    /// Inject the engine-bound [`SubagentRunner`] (built by the harness) and the
    /// parent's tool universe used for policy intersection.
    pub fn with_runner(
        mut self,
        runner: Arc<dyn SubagentRunner>,
        tool_universe: Vec<String>,
    ) -> Self {
        self.runner = Some(runner);
        self.tool_universe = tool_universe;
        self
    }

    /// Resolve a `subagent_type` to its [`AgentDefinition`] (built-ins for now;
    /// the YAML overlay is applied by the harness when it has a project root).
    pub fn resolve_definition(&self, subagent_type: &str) -> AgentDefinition {
        agent_definitions::resolve(subagent_type)
    }

    /// Resolve the agent definition's [`ToolPolicy`] into a concrete allow-list
    /// of tool names, intersected with the parent's tool universe.
    ///
    /// Returns an empty vec when the result is "everything the parent has"
    /// (i.e. `AllowAll` or an unknown universe), which the runner treats as "no
    /// restriction beyond the parent's registry".
    pub fn allowed_tools(&self, def: &AgentDefinition) -> Vec<String> {
        if self.tool_universe.is_empty() {
            return Vec::new();
        }
        match &def.tools {
            ToolPolicy::AllowAll => Vec::new(),
            ToolPolicy::AllowList { list } => self
                .tool_universe
                .iter()
                .filter(|t| list.contains(t))
                .cloned()
                .collect(),
            ToolPolicy::DenyList { list } => self
                .tool_universe
                .iter()
                .filter(|t| !list.contains(t))
                .cloned()
                .collect(),
        }
    }

    /// Build the `oh run …` argument vector for a subprocess/worktree spawn.
    ///
    /// Mirrors the flags `oh-harness`'s `run_once` accepts: `--prompt` plus the
    /// resolved agent-definition flags (`--agent-def`, `--system-prompt`,
    /// `--model`) and `--json` so the parent can parse `{"result": …}`.
    fn build_run_args(&self, req: &SpawnRequest, def: &AgentDefinition) -> Vec<String> {
        let mut args = vec![
            "run".to_string(),
            "--prompt".to_string(),
            req.prompt.clone(),
        ];
        args.push("--agent-def".to_string());
        args.push(def.name.clone());
        if let Some(sp) = &def.system_prompt {
            args.push("--system-prompt".to_string());
            args.push(sp.clone());
        }
        // Model precedence: explicit request model > agent-def model.
        if let Some(model) = req.model.clone().or_else(|| def.model.clone()) {
            args.push("--model".to_string());
            args.push(model);
        }
        args.push("--json".to_string());
        args
    }

    /// Render the `oh run …` invocation as a shell command string for the
    /// background task manager's `/bin/bash -lc` path. Each argument is
    /// single-quote-escaped so prompts with spaces/quotes survive.
    fn build_run_command(&self, req: &SpawnRequest, def: &AgentDefinition) -> String {
        let oh = std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "oh".to_string());
        let mut parts = vec![shell_quote(&oh)];
        for a in self.build_run_args(req, def) {
            parts.push(shell_quote(&a));
        }
        parts.join(" ")
    }

    /// Spawn a subprocess (or worktree) subagent: build the `oh run` command,
    /// record a `TaskType::RemoteAgent` task through `BackgroundTaskManager`
    /// (reusing its child-spawn + log-tee path so `read_output` returns the
    /// subagent's stdout), and — for worktree mode — create the worktree to use
    /// as cwd and schedule its removal on completion.
    async fn spawn_remote(
        &self,
        req: SpawnRequest,
        def: &AgentDefinition,
        worktree: bool,
    ) -> Result<SpawnResult, SubagentError> {
        let command = self.build_run_command(&req, def);
        let description = format!("subagent {} ({})", req.agent_id, def.name);

        let (cwd, worktree_dir) = if worktree {
            let repo = std::path::PathBuf::from(&self.cwd);
            let dir = oh_config::get_tasks_dir()
                .join("worktrees")
                .join(req.agent_id.as_str());
            oh_swarm::add_worktree(&repo, &dir, None)
                .await
                .map_err(|e| SubagentError::Spawn(format!("git worktree add: {e}")))?;
            (dir.to_string_lossy().into_owned(), Some((repo, dir)))
        } else {
            (self.cwd.clone(), None)
        };

        let record = self
            .tasks
            .create_remote_agent_task(&command, &description, &cwd)
            .await;

        // Clean up the worktree once the task reaches a terminal state.
        if let Some((repo, dir)) = worktree_dir {
            let target = record.id.clone();
            let unregister = self
                .tasks
                .register_completion_listener(std::sync::Arc::new(
                    move |rec: &oh_types::tasks::TaskRecord| {
                        if rec.id == target {
                            let repo = repo.clone();
                            let dir = dir.clone();
                            tokio::spawn(async move {
                                oh_swarm::remove_worktree(&repo, &dir).await;
                            });
                        }
                    },
                ))
                .await;
            // The listener self-fires on completion; drop the unregister handle
            // (it removes the closure on the next scheduler tick, which is fine
            // because the task is one-shot).
            drop(unregister);
        }

        Ok(SpawnResult {
            agent_id: req.agent_id,
            task_id: record.id,
        })
    }
}

#[async_trait]
impl SubagentSpawner for SubagentManager {
    async fn spawn(&self, req: SpawnRequest) -> Result<SpawnResult, SubagentError> {
        // Resolve the agent definition (validates the type via fallback).
        let def = self.resolve_definition(&req.subagent_type);

        // Resolve the effective backend mode (explicit request → env → default)
        // and ensure a backend can be constructed for it.
        let mode = self.backends.resolve_mode(req.isolation);
        let _backend = self.backends.backend_for(mode)?;

        // Subprocess / worktree spawns shell out to `oh run` and are tracked as
        // `RemoteAgent` tasks; only InProcess uses the nested-runner path below.
        match mode {
            SubagentIsolation::Subprocess => return self.spawn_remote(req, &def, false).await,
            SubagentIsolation::Worktree => return self.spawn_remote(req, &def, true).await,
            SubagentIsolation::InProcess => {}
        }

        let allowed = self.allowed_tools(&def);
        let description = format!("subagent {} ({})", req.agent_id, def.name);
        let agent_id = req.agent_id.clone();
        let prompt = req.prompt.clone();

        // Build the in-process job: run the engine-bound subagent (or echo when
        // no runner is injected), post the result as an `IdleNotification` to
        // the agent's mailbox, then return the final text for the task log.
        let team_root = self.team_root.clone();
        let teammate_id = TeammateId::new(agent_id.as_str());
        let runner = self.runner.clone();
        let run_req = req.clone();

        let job: crate::tasks::InProcessJob = Box::pin(async move {
            let result = match runner {
                Some(r) => r.run(run_req, allowed).await,
                None => Ok(prompt),
            };

            // Post the outcome to the agent's mailbox regardless of success so a
            // parent watching the mailbox always learns the spawn finished.
            let mailbox = Mailbox::for_agent(&team_root, &teammate_id);
            let (ok, body_text) = match &result {
                Ok(s) => (true, s.clone()),
                Err(e) => (false, e.clone()),
            };
            let msg = Message::new(
                teammate_id.clone(),
                teammate_id.clone(),
                MessageKind::IdleNotification,
                serde_json::json!({ "result": body_text, "stats": { "ok": ok } }),
            );
            if let Err(e) = mailbox.send(&msg).await {
                tracing::warn!("subagent {teammate_id}: mailbox send failed: {e}");
            }

            result
        });

        let record = self
            .tasks
            .create_in_process_task(&req.prompt, &description, &self.cwd, job)
            .await;

        Ok(SpawnResult {
            agent_id: req.agent_id,
            task_id: record.id,
        })
    }
}

/// Single-quote-escape `s` for a POSIX `/bin/bash -lc` command line.
///
/// Wraps the value in single quotes and replaces embedded single quotes with
/// the `'\''` idiom so prompts containing spaces, quotes, or shell
/// metacharacters are passed through verbatim.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
    async fn test_spawn_subprocess_records_remote_agent_task() {
        let mgr = manager();
        let mut req = SpawnRequest::new(AgentId::new("sub-2"), "worker", "go");
        req.isolation = SubagentIsolation::Subprocess;
        let res = mgr.spawn(req).await.unwrap();
        assert_eq!(res.agent_id, AgentId::new("sub-2"));

        // A RemoteAgent task is recorded (the child shells out to `oh run`; we
        // only assert the handle is uniform with in-process, not that the real
        // `oh` binary ran).
        let rec = mgr.tasks.get_task(&res.task_id).await.unwrap();
        assert_eq!(rec.task_type, oh_types::tasks::TaskType::RemoteAgent);
        assert!(rec.command.as_deref().unwrap().contains("--prompt"));
        assert!(rec.command.as_deref().unwrap().contains("'go'"));
        // Stop it so a slow/hanging child is reaped.
        mgr.tasks.stop_task(&res.task_id).await;
    }

    #[test]
    fn test_shell_quote_escapes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_build_run_args_includes_flags() {
        let mgr = manager();
        let def = mgr.resolve_definition("Explore");
        let req = SpawnRequest::new(AgentId::new("r1"), "Explore", "find things");
        let args = mgr.build_run_args(&req, &def);
        assert_eq!(args[0], "run");
        assert!(args.iter().any(|a| a == "--prompt"));
        assert!(args.iter().any(|a| a == "find things"));
        assert!(args.iter().any(|a| a == "--agent-def"));
        assert!(args.iter().any(|a| a == "Explore"));
        assert!(args.iter().any(|a| a == "--json"));
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

    #[test]
    fn test_allowed_tools_intersection() {
        let universe = vec![
            "bash".to_string(),
            "Edit".to_string(),
            "Write".to_string(),
            "Read".to_string(),
            "NotebookEdit".to_string(),
        ];
        let mgr = SubagentManager::new(Arc::new(BackgroundTaskManager::new()), "/tmp")
            .with_runner(Arc::new(EchoRunner), universe);

        // AllowAll → empty (no restriction).
        let gp = mgr.resolve_definition("general-purpose");
        assert!(mgr.allowed_tools(&gp).is_empty());

        // Explore is a DenyList of Edit/Write/NotebookEdit → keep the rest.
        let explore = mgr.resolve_definition("Explore");
        let mut allowed = mgr.allowed_tools(&explore);
        allowed.sort();
        assert_eq!(allowed, vec!["Read".to_string(), "bash".to_string()]);
    }

    #[tokio::test]
    async fn test_spawn_with_runner_writes_idle_notification() {
        let dir = tempfile::tempdir().unwrap();
        let team_root = dir.path().join("teammates");
        let mut mgr = SubagentManager::new(Arc::new(BackgroundTaskManager::new()), "/tmp")
            .with_runner(Arc::new(EchoRunner), Vec::new());
        mgr.team_root = team_root.clone();

        let req = SpawnRequest::new(AgentId::new("sub-mb"), "general-purpose", "ping");
        let res = mgr.spawn(req).await.unwrap();

        // Poll until the task completes.
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let r = mgr.tasks.get_task(&res.task_id).await.unwrap();
            if r.status != oh_types::tasks::TaskStatus::Running {
                break;
            }
        }

        // The IdleNotification landed in the agent's mailbox.
        let mailbox = Mailbox::for_agent(&team_root, &TeammateId::new("sub-mb"));
        let msgs = mailbox.peek_all().await.unwrap();
        assert_eq!(msgs.len(), 1, "expected one idle notification: {msgs:?}");
        assert_eq!(msgs[0].kind, MessageKind::IdleNotification);
        assert_eq!(msgs[0].body["result"], "echo:ping");

        // And the result is in the task log (read_output works).
        let out = mgr.tasks.read_output(&res.task_id, 65536).await.unwrap();
        assert_eq!(out, "echo:ping");
    }

    /// Test runner that prefixes the prompt — stands in for the engine-bound
    /// runner the harness injects.
    struct EchoRunner;
    #[async_trait]
    impl SubagentRunner for EchoRunner {
        async fn run(
            &self,
            req: SpawnRequest,
            _allowed_tools: Vec<String>,
        ) -> Result<String, String> {
            Ok(format!("echo:{}", req.prompt))
        }
    }
}
