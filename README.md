# ohrs

A Rust-based AI agent harness with deep lifecycle hooks, multi-provider LLM support, and full action trajectory recording.

## What makes this different

Most agent frameworks give you a loop: prompt the model, run tools, repeat. ohrs wraps **every internal event** — API calls, tool executions, permission checks, message pushes, session lifecycle — in a hook system that lets you observe, validate, or block any operation in real time.

Four hook types, each for a different integration pattern:

| Hook type | Runs | Default blocking | Use case |
|-----------|------|-------------------|----------|
| **Command** | Shell script | No | Logging, external scripts, CI gates |
| **HTTP** | POST to a URL | No | Webhooks, remote audit, analytics |
| **Prompt** | LLM call | Yes | Semantic validation, safety checks |
| **Agent** | LLM call (deep) | Yes | Complex policy, adversarial review |

Hooks fire at **42 lifecycle points** covering sessions, API calls, tool execution, permissions, messages, query turns, plugins, MCP, tasks, streaming, and more. Each receives the full event payload as JSON. Hooks can be configured in `settings.json`, loaded from plugins, or modified at runtime by the agent itself.

## Installation

### From source

```bash
git clone https://github.com/jouchuki/ohrs.git
cd ohrs
cargo build --release
```

The binary is at `target/release/oh`. Add it to your PATH:

```bash
cp target/release/oh ~/.local/bin/
```

### Dependencies

- Rust 1.75+ (2021 edition)
- OpenSSL or rustls (rustls used by default)

## Quick start

```bash
# Anthropic (default)
export ANTHROPIC_API_KEY=sk-ant-...
oh -p "explain this codebase"

# OpenAI
export OPENAI_API_KEY=sk-proj-...
export OPENHARNESSRS_PROVIDER=openai
oh -p "explain this codebase"

# Interactive TUI
oh

# With trajectory recording
oh -p "analyze this repo" --trajectory trace.jsonl

# Full auto (no permission prompts)
oh -p "fix the tests" --permission-mode full_auto
```

## Providers

Auto-detected from model name, base URL, or explicit config:

| Provider | Detection | Default model | Env var |
|----------|-----------|---------------|---------|
| Anthropic | `claude-*` models, default | `claude-sonnet-4-6` | `ANTHROPIC_API_KEY` |
| OpenAI | `gpt-*`, `o1/o3/o4` models | `gpt-5.4-2026-03-05` | `OPENAI_API_KEY` |

Switch provider explicitly:

```bash
export OPENHARNESSRS_PROVIDER=openai
```

Or in `~/.openharnessrs/settings.json`:

```json
{
  "provider": "openai",
  "model": "gpt-5.4-2026-03-05",
  "max_tokens": 16384
}
```

## Trajectory recording

Capture the complete agent action trace with `--trajectory`:

```bash
oh -p "investigate this bug" --trajectory trace.jsonl
```

Every event is recorded — streaming text fragments, full assistant responses with reasoning, tool dispatch with inputs, tool outputs, token usage, and wall-clock timing:

```jsonl
{"seq":0,"turn":0,"action":"text_delta","text":"Let me ","_t_ms":1200}
{"seq":1,"turn":0,"action":"text_delta","text":"look at the logs...","_t_ms":1250}
{"seq":2,"turn":1,"action":"assistant_response","reasoning":"Let me look at the logs...","tool_calls":[{"id":"call_1","name":"bash","input":{"command":"cat /var/log/app.log"}}],"content_blocks":[...],"usage":{"input_tokens":150,"output_tokens":42},"_t_ms":1400}
{"seq":3,"turn":1,"action":"tool_start","tool":"bash","input":{"command":"cat /var/log/app.log"},"_t_ms":1401}
{"seq":4,"turn":1,"action":"tool_result","tool":"bash","output":"ERROR: connection refused...","is_error":false,"_t_ms":1450}
{"seq":5,"turn":2,"action":"assistant_response","reasoning":"The log shows a connection refused error...","tool_calls":[],...,"_t_ms":2100}
{"seq":6,"action":"trajectory_end","total_turns":2,"total_events":6,"elapsed_ms":2100}
```

This is different from OTel telemetry (which captures operational metrics like latency histograms and token counters). Trajectories capture **what the agent did and why** — useful for debugging reasoning, reproducing runs, building eval datasets, and audit trails.

## Hooks

### Configuration

In `~/.openharnessrs/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "type": "command",
        "command": "echo \"Tool: $OPENHARNESS_HOOK_EVENT\" >> /tmp/audit.log",
        "timeout_seconds": 5,
        "block_on_failure": false
      }
    ],
    "PostApiResponse": [
      {
        "type": "http",
        "url": "https://your-webhook.example.com/events",
        "headers": { "Authorization": "Bearer ..." },
        "timeout_seconds": 10
      }
    ],
    "PreToolUse": [
      {
        "type": "prompt",
        "prompt": "Is this tool call safe? Tool: $ARGUMENTS",
        "block_on_failure": true,
        "matcher": "Bash"
      }
    ]
  }
}
```

### Hook events (42)

**Session**: `SessionStart`, `SessionEnd`, `SessionSave`, `SessionResume`

**API**: `PreApiRequest`, `PostApiResponse`, `ApiRetry`, `ApiError`

**Tools**: `PreToolUse`, `PostToolUse`, `ToolInputValidation`, `ToolTimeout`, `ToolError`

**Permissions**: `PrePermissionCheck`, `PostPermissionCheck`, `PermissionDenied`, `PermissionConfirmation`

**Messages**: `PreUserMessage`, `PostUserMessage`, `PrePushMessage`, `PostPushMessage`, `PreSystemPrompt`, `PostSystemPrompt`

**Query loop**: `QueryTurnStart`, `QueryTurnEnd`

**Commands**: `PreCommand`, `PostCommand`, `CommandExecuted`

**Streaming**: `StreamStart`, `StreamEnd`

**History**: `PreClearHistory`, `PostClearHistory`, `ContextCompacted`, `MemoryUpdated`

**Plugins & MCP**: `PluginLoaded`, `PluginUnloaded`, `McpConnected`, `McpDisconnected`

**Tasks**: `TaskCreated`, `TaskCompleted`

**Other**: `ErrorOccurred`, `SkillInvoked`

### Glob matching

Hooks support glob patterns to filter which events they respond to:

```json
{
  "type": "command",
  "command": "logger 'dangerous tool used'",
  "matcher": "Bash",
  "block_on_failure": false
}
```

The matcher checks `tool_name`, `prompt`, or `event` fields in the payload.

### Runtime hook modification

The agent can manage its own hooks via the `HookManage` tool — adding, clearing, or replacing hooks during execution.

## Permissions

Three modes controlling tool execution:

| Mode | Read-only tools | Mutating tools | Flag |
|------|----------------|----------------|------|
| **Default** | Allowed | Require confirmation | `--permission-mode default` |
| **Plan** | Allowed | Blocked | `--permission-mode plan` |
| **FullAuto** | Allowed | Auto-allowed | `--permission-mode full_auto` |

Fine-grained control via `settings.json`:

```json
{
  "permission": {
    "mode": "default",
    "allowed_tools": ["FileRead", "Grep", "Glob"],
    "denied_tools": ["Bash"],
    "denied_commands": ["rm -rf", "sudo"],
    "path_rules": [
      { "pattern": "/etc/**", "allow": false }
    ]
  }
}
```

## Tools

43+ built-in tools:

**File I/O**: `FileRead`, `FileWrite`, `FileEdit`, `Glob`, `Grep`

**Execution**: `Bash`, `Agent` (spawn sub-agents), `Sleep`

**Tasks**: `TaskCreate`, `TaskGet`, `TaskList`, `TaskUpdate`, `TaskStop`, `TaskOutput`

**Web**: `WebFetch`, `WebSearch`

**Scheduling**: `CronCreate`, `CronList`, `CronDelete`

**Development**: `EnterWorktree`, `ExitWorktree`, `EnterPlanMode`, `ExitPlanMode`, `NotebookEdit`, `Lsp`

**MCP**: `McpTool`, `McpAuth`, `ListMcpResources`, `ReadMcpResource`

**Meta**: `Skill`, `ToolSearch`, `Config`, `HookManage`, `AskUserQuestion`, `SendMessage`, `Brief`, `TodoWrite`, `RemoteTrigger`

## Plugins

### Static plugins (JSON + markdown)

```
~/.openharnessrs/plugins/my-plugin/
  plugin.json          # manifest
  skills/              # markdown skill definitions
    commit.md
    review.md
  hooks.json           # hook definitions
  mcp.json             # MCP server configs
```

### Native plugins (shared libraries)

Implement the `OpenHarnessPlugin` trait and use `#[openharness_plugin]` to generate the FFI boundary:

```rust
use oh_plugin_abi::traits::*;
use oh_plugin_derive::openharness_plugin;

#[openharness_plugin]
struct MyPlugin;

impl OpenHarnessPlugin for MyPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            name: "my-plugin".into(),
            version: "0.1.0".into(),
            description: "Does things".into(),
            enabled_by_default: true,
        }
    }
    fn init(&mut self, _config: serde_json::Value) -> Result<(), String> { Ok(()) }
    fn execute_command(&self, cmd: &str, args: serde_json::Value) -> Result<CommandResult, String> {
        Ok(CommandResult { output: "done".into(), is_error: false })
    }
}
```

Build as `cdylib` and drop the `.so` in `~/.openharnessrs/plugins/`.

## Telemetry

OpenTelemetry metrics for operational monitoring:

```bash
export OPENHARNESS_TELEMETRY=otlp
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
oh -p "do work"
```

Tracked metrics: `api_request_duration_seconds`, `tool_call_duration_seconds`, `token_usage_total`, `tool_error_total`, `hook_execution_duration_seconds`, `hook_blocked_total`, `permission_check_total`, `permission_denied_total`, `active_sessions`, `active_background_tasks`, `mcp_call_duration_seconds`.

## Architecture

```
oh-harness        CLI + TUI entry point
  oh-engine       Query loop, tool dispatch, hook firing
    oh-api        LLM clients (Anthropic, OpenAI) + streaming
    oh-tools      43+ tool implementations + registry
    oh-permissions Permission checker (3 modes)
    oh-hooks      Hook executor (command/http/prompt/agent)
    oh-plugins    Plugin discovery + dylib/JSON loading
    oh-services   Tasks, teams, cron, sessions
    oh-mcp        Model Context Protocol client
  oh-config       Settings, paths, env var resolution
  oh-telemetry    OTel metrics + tracing init
  oh-types        Shared types (messages, events, hooks, permissions)
  oh-plugin-abi   Plugin FFI boundary
  oh-plugin-derive Proc-macro for plugin codegen
```

## Configuration

All settings configurable via `~/.openharnessrs/settings.json`, environment variables, or CLI flags.

| Setting | Env var | CLI | Default |
|---------|---------|-----|---------|
| Provider | `OPENHARNESSRS_PROVIDER` | — | `anthropic` |
| Model | `ANTHROPIC_MODEL` / `OPENHARNESSRS_MODEL` | `--model` | `claude-sonnet-4-6` |
| API key | `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` | — | — |
| Base URL | `ANTHROPIC_BASE_URL` / `OPENHARNESSRS_BASE_URL` | — | provider default |
| Max tokens | `OPENHARNESSRS_MAX_TOKENS` | — | `16384` |
| Max turns | — | `--max-turns` | `30` |
| Permission mode | `OPENHARNESSRS_PERMISSION_MODE` | `--permission-mode` | `default` |
| Telemetry | `OPENHARNESS_TELEMETRY` | — | `off` |

## License

MIT
