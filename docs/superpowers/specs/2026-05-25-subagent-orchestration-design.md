# Subagent Orchestration — Design Spec

Date: 2026-05-25
Status: approved, in implementation

## Goal

Implement real subagent orchestration for `ohrs`. Today every spawn path is a
stub: `AgentTool` formats a string, `TaskCreate` returns `{"id":"pending"}`,
`BackgroundTaskManager::create_agent_task` `echo`s the prompt, and the
fully-built `oh-swarm` crate is a dependency of nothing.

## Core principle — one action control plane; a subagent is a nested `run_query`

Every controllable action is a hook-gated, permission-checked, recorded event
that flows through the existing pipeline in `oh-engine/src/query.rs`
(`execute_tool_call`: `PreToolUse` → `PrePermissionCheck` → `PermissionChecker::evaluate`
→ `PostToolUse`/`ToolError`). A subagent is simply `run_query` invoked with its
own `QueryContext` that carries the **same** `hook_executor` + `permission_checker`
plus an agent identity and a trajectory recorder. Subagents therefore inherit
blocks, permissions, webhooks, and recording for free — nothing is special-cased.

Mapping of the four control requirements to existing machinery:

- **Blocks** → `PreToolUse` (exists) + new `SubagentStart` hook; `block_on_failure`
  already aborts the action (`oh-hooks/src/executor.rs`).
- **Permissions** → each subagent's `QueryContext` gets a `PermissionChecker` and a
  tool-registry view scoped by its `AgentDefinition` `ToolPolicy`
  (`AllowAll`/`AllowList`/`DenyList`, already defined in
  `oh-services/src/coordinator/agent_definitions.rs`). The set is the agent-def
  policy intersected with the parent's allowed tools.
- **Webhooks** → oh-hooks already has **HTTP hooks** (`HttpHookDefinition`,
  `run_http_hook`). They fire on any `HookEvent`, including the new `Subagent*`
  events. No new webhook code beyond the events.
- **Trajectories** → each agent run is a parent-linked SQLite `Session`
  (`oh-services/src/sessions/mod.rs`: `SessionStore`, `SessionRecord`,
  `SessionMessage`). Recording is implemented **as a recording hook** so it is the
  same mechanism as webhooks/blocks.

## Hard constraints for all implementers

- **Follow the existing repo structure. Do not invent new crates, abstractions,
  or patterns.** Mirror the conventions of neighboring code: `thiserror` error
  enums, `async_trait` traits, trait objects defined in `oh-types`
  (`StreamingApiClient`, `HookExecutorTrait` are the precedent), `#[cfg(test)]`
  unit-test modules per file.
- Reuse existing types. `TaskType` already has `LocalBash`, `LocalAgent`,
  `RemoteAgent`, `InProcessTeammate` — do not add variants.
- No dependency cycles. `oh-types` is the root crate; service handles reach tools
  via trait objects defined in `oh-types`, never by `oh-types` depending on
  `oh-services`.
- Every new public item gets at least one unit test, matching the density of the
  file it lives in.
- Keep `cargo fmt` clean and `cargo clippy --workspace --all-targets -- -D warnings`
  passing **for newly-written code** (pre-existing tree warnings are out of scope).

## Components

### oh-types (Phase 0)
- `QueryContext`-supporting identity types: `AgentId(String)`, `parent_id:
  Option<AgentId>`, `session_id: Option<String>` will be added to
  `oh-engine`'s `QueryContext` (the struct lives in `oh-engine/src/query.rs`;
  the id newtype lives in `oh-types`).
- `HookEvent` (in `oh-types/src/hooks.rs`) gains `SubagentStart`, `SubagentStop`,
  `SubagentMessage`.
- New trait objects so tools can reach services without a dependency cycle
  (mirror `StreamingApiClient`/`HookExecutorTrait`):
  - `trait SubagentSpawner` — `async fn spawn(&self, req: SpawnRequest) -> SpawnResult`.
  - `trait BackgroundTasks` — create/get/list/stop/read-output over `TaskRecord`.
  - `SpawnRequest`/`SpawnResult` value types (`agent_id`, `task_id`,
    `subagent_type`, `prompt`, `model`, `run_in_background`, `isolation`).
- `ToolExecutionContext` gains `pub subagents: Option<Arc<dyn SubagentSpawner>>`
  and `pub tasks: Option<Arc<dyn BackgroundTasks>>` (default `None`; existing
  `ToolExecutionContext::new` stays source-compatible).

### oh-engine (Phase 0 + Phase 1)
- `QueryContext` gains `agent_id: AgentId`, `parent_id: Option<AgentId>`,
  `session_id: Option<String>`, `recorder: Option<Arc<dyn HookExecutorTrait>>`
  reuse — recording rides the hook executor, so no separate field if the
  recorder is registered as a hook. (Decision: recording is a hook.)
- New `pub async fn run_subagent(ctx: QueryContext, prompt: String) ->
  Result<String, EngineError>`: seeds the message list with the prompt, calls
  `run_query`, returns the final assistant text. Fires `SubagentStart` before and
  `SubagentStop` after (with result/stats payload).

### oh-swarm (Phase 2 + Phase 3)
- `MessageKind` gains `IdleNotification` carrying `{ result: String, stats:
  serde_json::Value }`.
- `SubprocessBackend` implementing the existing `Backend` trait: spawns the `oh`
  binary in one-shot mode, prompt passed via flags. Tracks output via the task
  log file.
- `WorktreeBackend`: `SubprocessBackend` whose cwd is a freshly-created
  `git worktree`, removed on completion. Maps to `IsolationMode::Worktree`.

### oh-services (Phase 0 skeleton, filled across phases)
- New module `subagent/` with:
  - `SubagentManager` implementing `oh_types::SubagentSpawner`. Resolves
    `subagent_type` → `AgentDefinition` (built-ins layered under the existing
    YAML loader), selects a backend via `BackendRegistry`, spawns, and **records a
    `TaskRecord` for every backend** through `BackgroundTaskManager` so handles are
    uniform. Returns `{agent_id, task_id}`.
  - `BackendRegistry`: selects InProcess / Subprocess / Worktree. Selection order:
    explicit `isolation`/mode arg → env override (`OPENHARNESSRS_TEAMMATE_MODE`,
    matching the existing `OPENHARNESSRS_*` env convention) → default InProcess.
  - Built-in `AgentDefinition`s registered in code: `general-purpose` (AllowAll),
    `Explore` (DenyList: Edit, Write, NotebookEdit), `Plan` (same read-only
    denylist), `worker` (AllowAll). Layer the YAML loader on top; YAML overrides
    built-ins by name.
- `BackgroundTaskManager` (`oh-services/src/tasks/manager.rs`):
  - Implement `oh_types::BackgroundTasks` for it.
  - Replace the `create_agent_task` `echo` stub: for InProcess spawn a tokio task
    running `run_subagent` and tee its streamed output to the task log; for
    Subprocess spawn the `oh` one-shot child (reuse the existing
    `spawn_and_watch`).
- `TrajectoryRecorder`: a `HookExecutorTrait`-compatible recorder that, on
  message/tool events, appends `SessionMessage`s to `SessionStore` for the agent's
  `session_id`. `SessionRecord` gets a `parent_session_id` link (additive column;
  follow the existing SQLite migration style in the module).
- Bridge `coordinator::TeamRegistry` (in-memory) as a view synced from
  `oh-swarm::TeamManager` on team create/add/remove (Phase 3).

### oh-tools (Phase 1 + Phase 3)
- `AgentTool::execute` → call `ctx.subagents` (`SubagentSpawner`); if absent,
  return a clear "subagent spawning not available" error. Return JSON
  `{agent_id, task_id}`. Always background per design (returns handle immediately).
- `TaskCreate`/`TaskGet`/`TaskList`/`TaskStop`/`TaskOutput` → call `ctx.tasks`
  (`BackgroundTasks`) instead of the `metadata["task_manager"]` placeholder.
- New message-send tool (Phase 3) fires `SubagentMessage` and writes to the
  recipient's `Mailbox`.

### oh-harness (Phase 2)
- One-shot CLI subcommand: `oh run --prompt <p> [--system-prompt <s>] [--model
  <m>] [--agent-def <name>] [--json]`. Builds a `QueryContext` and calls
  `run_subagent`, printing the final result (JSON when `--json`). This is what
  `SubprocessBackend` invokes. Follow the existing `clap` derive structure in
  `oh-harness/src/cli.rs`.
- Construct `SubagentManager` + `BackgroundTaskManager`, wrap as the
  `SubagentSpawner`/`BackgroundTasks` trait objects, and inject into the
  `ToolExecutionContext` used by the engine.

## The three backends (all behind the existing `Backend` trait, all recorded as `TaskRecord`s)

- **InProcess** (Phase 1) — tokio task runs `run_subagent`; shares api_client/config;
  `Mailbox` for messages.
- **Subprocess** (Phase 2) — `oh run` child; prompt via flags; output tee'd to task log.
- **Worktree** (Phase 3) — subprocess in a fresh `git worktree`, cleaned on completion.

## Phasing (dependency-ordered)

- **Phase 0 — Foundations (gate).** oh-types trait objects + context fields +
  `HookEvent` variants + `AgentId`; `QueryContext` identity fields; `run_subagent`
  signature (may return `unimplemented` body until Phase 1 — but must compile and
  fire Start/Stop); `SubagentManager` + `BackendRegistry` skeleton with built-in
  agent defs; `TrajectoryRecorder` + `parent_session_id`; impl `BackgroundTasks`
  for `BackgroundTaskManager`. **Everything compiles; tests pass.** Nothing below
  starts until this merges.
- **Phase 1 — In-process end-to-end.** `run_subagent` body; InProcess spawn via
  `SubagentManager`; `AgentTool` real; `TaskCreate`/Get/List/Stop/Output real;
  trajectory recorded; **prove a webhook fires and a block aborts a subagent action.**
- **Phase 2 — Subprocess.** `oh run` CLI; `SubprocessBackend`; real
  `create_agent_task` subprocess path.
- **Phase 3 — Worktree + team bridge + inter-agent messaging tool.**
- **Deferred (not this spec):** leader/worker permission-sync protocol; tmux/pane backend.

## Out of scope

- Permission request/response mailbox protocol (subagents use static allowlists).
- tmux/iTerm2 pane backend.
- Streaming stdin to subprocess agents (flags only for now).
