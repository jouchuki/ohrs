//! CLI argument definitions and main entry point.

use clap::{Parser, Subcommand};
use oh_api::{AnthropicApiClient, CodexApiClient, OpenAiApiClient, StreamingApiClient};
use oh_config::{load_settings, CliOverrides};
use oh_engine::QueryEngine;
use oh_hooks::executor::{HookExecutionContext, HookExecutor};
use oh_hooks::loader::HookRegistry;
use oh_hooks::{HookEvent, HookExecutorTrait};
use oh_permissions::PermissionChecker;
use oh_tools::create_default_tool_registry;
use std::path::PathBuf;
use std::sync::Arc;

/// Fans a lifecycle event out to two [`HookExecutorTrait`]s: the real hook
/// executor (blocks/webhooks) and the main agent's trajectory recorder.
///
/// Mirrors `subagent_runner::FanoutHookExecutor`. A block from the inner
/// executor still aborts the action; the recorder never blocks.
struct FanoutHookExecutor {
    inner: Arc<dyn HookExecutorTrait>,
    recorder: Arc<dyn HookExecutorTrait>,
}

#[async_trait::async_trait]
impl HookExecutorTrait for FanoutHookExecutor {
    async fn execute(
        &self,
        event: HookEvent,
        payload: serde_json::Value,
    ) -> oh_hooks::AggregatedHookResult {
        // Record first (never blocks), then run the real hooks (may block).
        let _ = self.recorder.execute(event, payload.clone()).await;
        self.inner.execute(event, payload).await
    }
}

/// ohrs — an AI-powered coding assistant
#[derive(Parser, Debug)]
#[command(name = "openharness", version, about)]
pub struct Args {
    /// Continue the most recent session
    #[arg(short = 'c', long = "continue")]
    pub continue_session: bool,

    /// Resume a specific session by ID or name
    #[arg(short = 'r', long)]
    pub resume: Option<String>,

    /// Session name
    #[arg(short = 'n', long)]
    pub name: Option<String>,

    /// Model to use
    #[arg(short = 'm', long)]
    pub model: Option<String>,

    /// Effort level
    #[arg(long)]
    pub effort: Option<String>,

    /// Maximum turns
    #[arg(long)]
    pub max_turns: Option<u32>,

    /// Print mode: provide a prompt and exit
    #[arg(short = 'p', long = "print")]
    pub prompt: Option<String>,

    /// Output format (text or json)
    #[arg(long, default_value = "text")]
    pub output_format: String,

    /// Permission mode (default, plan, full_auto)
    #[arg(long)]
    pub permission_mode: Option<String>,

    /// Skip all permission checks (dangerous!)
    #[arg(long)]
    pub dangerously_skip_permissions: bool,

    /// Override system prompt
    #[arg(short = 's', long)]
    pub system_prompt: Option<String>,

    /// Append to system prompt
    #[arg(long)]
    pub append_system_prompt: Option<String>,

    /// Path to settings file
    #[arg(long)]
    pub settings: Option<String>,

    /// Enable debug logging
    #[arg(short = 'd', long)]
    pub debug: bool,

    /// MCP config file path
    #[arg(long)]
    pub mcp_config: Option<String>,

    /// Bare mode (no CLAUDE.md, no memory)
    #[arg(long)]
    pub bare: bool,

    /// Save full action trajectory to a JSONL file
    #[arg(long)]
    pub trajectory: Option<String>,

    /// Set the working directory for this run (cwd is normally inherited from the parent).
    /// All file tools, the bash tool, and hooks will execute from this directory.
    /// Mutually exclusive with --tempdir.
    #[arg(long, conflicts_with = "tempdir")]
    pub cwd: Option<String>,

    /// Create a fresh temporary directory and use it as cwd for this run.
    /// The directory is deleted automatically when the process exits.
    /// Mutually exclusive with --cwd.
    #[arg(long)]
    pub tempdir: bool,

    /// One-shot / non-interactive subcommands.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level subcommands. When absent, the binary runs in print/interactive mode.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run a single prompt to completion and print the result (used by the
    /// subprocess subagent backend).
    Run(crate::run_once::RunArgs),
}

/// Main CLI entry point.
pub async fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    // One-shot subcommands short-circuit the interactive/print path.
    if let Some(Command::Run(run_args)) = args.command {
        return crate::run_once::run(run_args).await;
    }

    // Load settings
    let config_path = args.settings.as_deref().map(PathBuf::from);
    let settings = load_settings(config_path.as_deref())?;

    // Apply CLI overrides
    let settings = settings.merge_cli_overrides(CliOverrides {
        model: args.model,
        max_tokens: None,
        base_url: None,
        system_prompt: args.system_prompt.clone(),
        api_key: None,
    });

    // Resolve API key (empty string for Codex ChatGPT OAuth — tokens are in env).
    let api_key = settings.resolve_api_key()?;

    // Create API client — pick provider automatically.
    let api_client: Arc<dyn StreamingApiClient> = if settings.is_codex() {
        tracing::info!(provider = "openai-codex", model = %settings.model, "Using Codex ChatGPT provider");
        let client = CodexApiClient::from_env().map_err(|e| {
            format!(
                "Codex provider requires CODEX_ACCESS_TOKEN and CODEX_REFRESH_TOKEN env vars: {e}"
            )
        })?;
        Arc::new(client)
    } else if settings.is_openai() {
        tracing::info!(provider = "openai", model = %settings.model, "Using OpenAI provider");
        Arc::new(OpenAiApiClient::new(&api_key, settings.base_url.as_deref()))
    } else {
        tracing::info!(provider = "anthropic", model = %settings.model, "Using Anthropic provider");
        Arc::new(AnthropicApiClient::new(
            &api_key,
            settings.base_url.as_deref(),
        ))
    };

    // Create permission checker — apply CLI override for permission mode
    let mut perm_settings = settings.permission.clone();
    if args.dangerously_skip_permissions {
        perm_settings.mode = oh_types::permissions::PermissionMode::FullAuto;
    } else if let Some(ref mode) = args.permission_mode {
        match mode.as_str() {
            "full_auto" | "auto" => {
                perm_settings.mode = oh_types::permissions::PermissionMode::FullAuto
            }
            "plan" => perm_settings.mode = oh_types::permissions::PermissionMode::Plan,
            _ => {} // keep default
        }
    }
    let permission_checker = Arc::new(PermissionChecker::new(perm_settings));

    // Create hook registry and executor
    let mut hook_registry = HookRegistry::new();
    hook_registry.merge_from_map(&settings.hooks);

    // Resolve cwd:
    //   --cwd <path>  → use that path
    //   --tempdir     → fresh tempdir, RAII-cleaned on process exit
    //   neither       → inherit from parent
    let (cwd, _tempdir_guard) = if let Some(custom) = args.cwd.as_deref() {
        let p = PathBuf::from(custom);
        std::fs::create_dir_all(&p).ok();
        (p, None)
    } else if args.tempdir {
        let td = tempfile::TempDir::new()?;
        let p = td.path().to_path_buf();
        (p, Some(td))
    } else {
        (
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            None,
        )
    };
    // Make subprocesses + relative-path tools inherit it.
    std::env::set_current_dir(&cwd).ok();

    // Load plugins
    let plugins = oh_plugins::load_all_plugins(&cwd, &settings.enabled_plugins);
    for plugin in &plugins {
        if plugin.enabled {
            hook_registry.merge_from_map(&plugin.hooks);
            tracing::info!(plugin = %plugin.name(), "Loaded plugin");
        }
    }

    let hook_executor = Arc::new(HookExecutor::new(
        hook_registry,
        HookExecutionContext {
            cwd: cwd.clone(),
            api_client: api_client.clone(),
            default_model: settings.model.clone(),
        },
    ));

    // ── Trajectory recording for the MAIN agent ────────────────────────────
    // Open the session store, create a MAIN session row, and fan a
    // TrajectoryRecorder for it into the hook executor the engine uses. The
    // runner keeps the BASE executor as its parent (so child events are not
    // double-recorded into the main session), and stamps every child session's
    // `parent_session_id` with the main session id.
    let session_store: Option<Arc<oh_services::sessions::SessionStore>> =
        match oh_services::sessions::SessionStore::with_default_backend().await {
            Ok(store) => Some(Arc::new(store)),
            Err(e) => {
                tracing::warn!("subagent: session store unavailable, trajectories disabled: {e}");
                None
            }
        };

    // Main session id: the resumed/-r id if present, else a fresh uuid.
    let main_session_id = args
        .resume
        .clone()
        .unwrap_or_else(|| format!("session-{}", uuid::Uuid::new_v4()));

    // The hook executor handed to the engine. When recording is on, this is the
    // base executor fanned together with the main trajectory recorder.
    let engine_hook_executor: Arc<dyn HookExecutorTrait> = if let Some(store) = &session_store {
        let rec = oh_services::sessions::SessionRecord {
            id: main_session_id.clone(),
            name: args.name.clone(),
            project_root: cwd.clone(),
            model: settings.model.clone(),
            created_at: std::time::SystemTime::now(),
            updated_at: std::time::SystemTime::now(),
            message_count: 0,
            status: oh_services::sessions::SessionStatus::Active,
            parent_session_id: None,
        };
        if let Err(e) = store.create_session(&rec).await {
            // A resumed session already exists; that is fine.
            tracing::debug!("main session create skipped: {e}");
        }
        let recorder: Arc<dyn HookExecutorTrait> =
            Arc::new(oh_services::subagent::TrajectoryRecorder::new(
                Arc::clone(store),
                main_session_id.clone(),
            ));
        Arc::new(FanoutHookExecutor {
            inner: hook_executor.clone(),
            recorder,
        })
    } else {
        hook_executor.clone()
    };

    // Build system prompt — base template + env + CLAUDE.md walk; honours --system-prompt / --append-system-prompt / --bare.
    let system_prompt: String = oh_services::prompts::PromptBuilder::new(&cwd)
        .with_override(args.system_prompt.as_deref())
        .with_append(args.append_system_prompt.as_deref())
        .bare(args.bare)
        .build();

    // Collect plugin skills before building registry
    let mut skill_registry_map = serde_json::Map::new();
    let mut skill_entries: Vec<oh_tools::skill::SkillEntry> = Vec::new();

    for plugin in &plugins {
        if !plugin.enabled {
            continue;
        }
        for skill in &plugin.skills {
            skill_registry_map.insert(
                skill.name.clone(),
                serde_json::json!({ "content": skill.content }),
            );
            skill_entries.push(oh_tools::skill::SkillEntry {
                name: skill.name.clone(),
                description: skill.description.clone(),
            });
        }
    }

    // Create tool registry — populate SkillTool with known skills before
    // it gets serialized into the API tool schema
    let skill_tool = oh_tools::skill::SkillTool::new();
    if !skill_entries.is_empty() {
        skill_tool.set_available_skills(skill_entries.clone());
        tracing::info!(
            "Registered {} plugin skills into Skill tool schema",
            skill_entries.len()
        );
    }

    let mut tool_registry = create_default_tool_registry();
    // Replace the default empty SkillTool with our populated one
    tool_registry.register(Box::new(skill_tool));
    // Capture the parent's tool universe for subagent tool-policy intersection.
    let tool_universe = tool_registry.tool_names();
    let tool_registry = Arc::new(tool_registry);

    // Clone the pieces the subagent runner needs before they are moved into the
    // engine constructor below.
    let runner_api_client = api_client.clone();
    let runner_permission_checker = permission_checker.clone();
    let runner_cwd = cwd.clone();
    let runner_system_prompt = system_prompt.clone();

    let system_prompt_copy = system_prompt.clone();
    let mut engine = QueryEngine::new(
        api_client,
        tool_registry,
        permission_checker,
        cwd,
        settings.model.clone(),
        system_prompt,
        settings.max_tokens,
    );
    engine.set_hook_executor(engine_hook_executor.clone());
    engine.set_session_id(main_session_id.clone());

    // Store skill content in engine metadata for execute() lookup
    if !skill_registry_map.is_empty() {
        engine.set_tool_metadata(
            "skill_registry".to_string(),
            serde_json::Value::Object(skill_registry_map),
        );
    }

    // Apply max_turns: CLI flag > settings.json > default (30)
    let max_turns = args.max_turns.unwrap_or(settings.max_turns);
    engine.set_max_turns(max_turns);

    // ── Subagent orchestration wiring ──────────────────────────────────────
    // Build the in-process control plane: a background-task manager, an
    // engine-bound runner, and the SubagentManager that ties them together.
    // Inject the two trait objects (SubagentSpawner / BackgroundTasks) into the
    // engine so the `Agent` and `Task*` tools can reach them from inside the
    // query loop. The session store + main session id were set up above.
    let task_manager = Arc::new(oh_services::tasks::BackgroundTaskManager::new());

    let runner = Arc::new(crate::subagent_runner::EngineSubagentRunner::new(
        runner_api_client,
        runner_permission_checker,
        // The runner's parent executor is the BASE one (no main recorder), so
        // child events are recorded only into the child session, not the main.
        Some(hook_executor.clone()),
        session_store.clone(),
        oh_types::subagent::AgentId::new("main"),
        // Stamp every spawned child session's parent_session_id with the main
        // session id, linking the full transcript tree.
        Some(main_session_id.clone()),
        runner_cwd,
        settings.model.clone(),
        runner_system_prompt,
        settings.max_tokens,
        max_turns,
    ));

    let subagent_manager = Arc::new(
        oh_services::subagent::SubagentManager::new(Arc::clone(&task_manager), ".".to_string())
            .with_runner(runner, tool_universe),
    );

    engine.set_subagents(subagent_manager);
    engine.set_tasks(task_manager);

    // Fire SessionStart hook
    hook_executor
        .execute(
            HookEvent::SessionStart,
            serde_json::json!({"model": settings.model}),
        )
        .await;

    // Print mode: submit prompt and exit
    if let Some(prompt) = args.prompt {
        let events = engine.submit_message(&prompt).await?;
        let mut printed_text = false;
        for (event, _) in &events {
            match event {
                oh_types::stream_events::StreamEvent::AssistantTextDelta(delta) => {
                    print!("{}", delta.text);
                    printed_text = true;
                }
                oh_types::stream_events::StreamEvent::AssistantTurnComplete(turn) => {
                    if !printed_text {
                        // API client returned complete message without streaming deltas
                        print!("{}", turn.message.text());
                    }
                    println!();
                }
                _ => {}
            }
        }
        // Save trajectory if requested
        if let Some(ref traj_path) = args.trajectory {
            let tool_schemas = engine.tool_schemas();
            save_trajectory(
                traj_path,
                &system_prompt_copy,
                &prompt,
                &settings.model,
                &tool_schemas,
                &events,
            )?;
            eprintln!("[trajectory saved to {traj_path}]");
        }
        // Fire SessionEnd hook before exiting print mode
        hook_executor
            .execute(
                HookEvent::SessionEnd,
                serde_json::json!({"reason": "print_mode_complete"}),
            )
            .await;
        return Ok(());
    }

    // Interactive TUI mode
    let perm_mode_display = format!("{}", settings.permission.mode);
    crate::ui::app::run_tui(
        engine,
        hook_executor,
        settings.model.clone(),
        perm_mode_display,
    )
    .await?;

    Ok(())
}

/// Serialize a training-ready trajectory to JSONL.
///
/// Output format follows the chat completions message schema so it can
/// be fed directly into fine-tuning pipelines:
///
/// ```jsonl
/// {"role":"system","content":"..."}
/// {"role":"user","content":"..."}
/// {"role":"assistant","content":"reasoning...","tool_calls":[{"id":"...","type":"function","function":{"name":"bash","arguments":"{...}"}}]}
/// {"role":"tool","tool_call_id":"...","name":"bash","content":"output..."}
/// {"role":"assistant","content":"more reasoning..."}
/// ```
///
/// Metadata (model, timing, token usage) is attached as `_meta` on
/// assistant messages so it can be stripped for training but kept for
/// analysis.
fn save_trajectory(
    path: &str,
    system_prompt: &str,
    user_message: &str,
    model: &str,
    tool_schemas: &[serde_json::Value],
    events: &[(
        oh_types::stream_events::StreamEvent,
        Option<oh_types::api::UsageSnapshot>,
    )],
) -> Result<(), Box<dyn std::error::Error>> {
    use oh_types::messages::ContentBlock;
    use oh_types::stream_events::StreamEvent;
    use std::io::Write;

    let file = std::fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    let start = std::time::Instant::now();

    let writeln_json = |writer: &mut std::io::BufWriter<std::fs::File>,
                        entry: serde_json::Value|
     -> Result<(), Box<dyn std::error::Error>> {
        serde_json::to_writer(&mut *writer, &entry)?;
        writeln!(writer)?;
        Ok(())
    };

    // ── System prompt ──
    writeln_json(
        &mut writer,
        serde_json::json!({
            "role": "system",
            "content": system_prompt,
            "_meta": {
                "model": model,
                "timestamp": chrono_now(),
                "tools": tool_schemas,
            }
        }),
    )?;

    // ── User message ──
    writeln_json(
        &mut writer,
        serde_json::json!({
            "role": "user",
            "content": user_message,
        }),
    )?;

    // ── Agent turns ──
    // Track pending tool_call_ids so we can pair ToolExecutionCompleted
    // with the correct call (supports parallel tool calls).
    let mut pending_tool_ids: std::collections::VecDeque<(String, String)> =
        std::collections::VecDeque::new();

    for (event, usage) in events {
        match event {
            StreamEvent::AssistantTurnComplete(tc) => {
                // Build content string from text blocks
                let content: String = tc
                    .message
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text(t) if !t.text.is_empty() => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");

                // Build tool_calls array in OpenAI fine-tuning format
                let tool_calls: Vec<serde_json::Value> = tc.message.content.iter().filter_map(|b| {
                    match b {
                        ContentBlock::ToolUse(tu) => {
                            // Queue the id so we can match it to ToolExecutionCompleted
                            pending_tool_ids.push_back((tu.name.clone(), tu.id.clone()));
                            Some(serde_json::json!({
                                "id": tu.id,
                                "type": "function",
                                "function": {
                                    "name": tu.name,
                                    "arguments": serde_json::to_string(&tu.input).unwrap_or_default(),
                                }
                            }))
                        }
                        _ => None,
                    }
                }).collect();

                let mut msg = serde_json::json!({
                    "role": "assistant",
                });

                // content is null when there's only tool calls (per OpenAI spec)
                if content.is_empty() {
                    msg["content"] = serde_json::Value::Null;
                } else {
                    msg["content"] = serde_json::json!(content);
                }

                if !tool_calls.is_empty() {
                    msg["tool_calls"] = serde_json::json!(tool_calls);
                }

                // Attach metadata for analysis (strip for training)
                msg["_meta"] = serde_json::json!({
                    "usage": usage,
                    "_t_ms": start.elapsed().as_millis() as u64,
                });

                writeln_json(&mut writer, msg)?;
            }

            StreamEvent::ToolExecutionCompleted(tc) => {
                // Pop the matching tool_call_id from the queue
                let tool_call_id = pending_tool_ids
                    .iter()
                    .position(|(name, _)| name == &tc.tool_name)
                    .and_then(|i| pending_tool_ids.remove(i))
                    .map(|(_, id)| id)
                    .unwrap_or_default();

                writeln_json(
                    &mut writer,
                    serde_json::json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "name": tc.tool_name,
                        "content": tc.output,
                        "_meta": {
                            "is_error": tc.is_error,
                            "_t_ms": start.elapsed().as_millis() as u64,
                        }
                    }),
                )?;
            }

            // Text deltas and tool starts are intermediate signals —
            // the complete data is in AssistantTurnComplete and
            // ToolExecutionCompleted above. Skip to keep the training
            // format clean.
            StreamEvent::AssistantTextDelta(_) => {}
            StreamEvent::ToolExecutionStarted(_) => {}
        }
    }

    Ok(())
}

fn chrono_now() -> String {
    use std::time::SystemTime;
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    // ISO-8601 approximation without pulling in chrono
    let days = secs / 86400;
    let years = 1970 + days / 365;
    let rem_days = days % 365;
    let months = rem_days / 30 + 1;
    let day = rem_days % 30 + 1;
    let hour = (secs % 86400) / 3600;
    let min = (secs % 3600) / 60;
    let sec = secs % 60;
    format!("{years:04}-{months:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}
