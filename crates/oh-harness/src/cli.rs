//! CLI argument definitions and main entry point.

use clap::Parser;
use oh_api::{AnthropicApiClient, OpenAiApiClient, StreamingApiClient};
use oh_config::{load_settings, CliOverrides};
use oh_engine::QueryEngine;
use oh_hooks::executor::{HookExecutionContext, HookExecutor};
use oh_hooks::loader::HookRegistry;
use oh_hooks::{HookEvent, HookExecutorTrait};
use oh_permissions::PermissionChecker;
use oh_tools::create_default_tool_registry;
use std::path::PathBuf;
use std::sync::Arc;

/// OpenHarness — an AI-powered coding assistant
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
}

/// Main CLI entry point.
pub async fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    // Load settings
    let config_path = args.settings.as_deref().map(PathBuf::from);
    let settings = load_settings(config_path.as_ref().map(|p| p.as_path()))?;

    // Apply CLI overrides
    let settings = settings.merge_cli_overrides(CliOverrides {
        model: args.model,
        max_tokens: None,
        base_url: None,
        system_prompt: args.system_prompt.clone(),
        api_key: None,
    });

    // Resolve API key
    let api_key = settings.resolve_api_key()?;

    // Create API client — pick provider automatically
    let api_client: Arc<dyn StreamingApiClient> = if settings.is_openai() {
        tracing::info!(provider = "openai", model = %settings.model, "Using OpenAI provider");
        Arc::new(OpenAiApiClient::new(&api_key, settings.base_url.as_deref()))
    } else {
        tracing::info!(provider = "anthropic", model = %settings.model, "Using Anthropic provider");
        Arc::new(AnthropicApiClient::new(&api_key, settings.base_url.as_deref()))
    };

    // Create permission checker — apply CLI override for permission mode
    let mut perm_settings = settings.permission.clone();
    if args.dangerously_skip_permissions {
        perm_settings.mode = oh_types::permissions::PermissionMode::FullAuto;
    } else if let Some(ref mode) = args.permission_mode {
        match mode.as_str() {
            "full_auto" | "auto" => perm_settings.mode = oh_types::permissions::PermissionMode::FullAuto,
            "plan" => perm_settings.mode = oh_types::permissions::PermissionMode::Plan,
            _ => {} // keep default
        }
    }
    let permission_checker = Arc::new(PermissionChecker::new(perm_settings));

    // Create hook registry and executor
    let mut hook_registry = HookRegistry::new();
    hook_registry.merge_from_map(&settings.hooks);

    // Load plugins
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
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

    // Build system prompt
    let system_prompt = args
        .system_prompt
        .unwrap_or_else(|| "You are a helpful AI coding assistant.".into());

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
        tracing::info!("Registered {} plugin skills into Skill tool schema", skill_entries.len());
    }

    let mut tool_registry = create_default_tool_registry();
    // Replace the default empty SkillTool with our populated one
    tool_registry.register(Box::new(skill_tool));
    let tool_registry = Arc::new(tool_registry);

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
    engine.set_hook_executor(hook_executor.clone());

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
            save_trajectory(
                traj_path,
                &system_prompt_copy,
                &prompt,
                &settings.model,
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
    crate::ui::app::run_tui(engine, hook_executor, settings.model.clone(), perm_mode_display).await?;

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
    events: &[(oh_types::stream_events::StreamEvent, Option<oh_types::api::UsageSnapshot>)],
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use oh_types::stream_events::StreamEvent;
    use oh_types::messages::ContentBlock;

    let file = std::fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    let start = std::time::Instant::now();

    let mut writeln_json = |writer: &mut std::io::BufWriter<std::fs::File>,
                            entry: serde_json::Value|
     -> Result<(), Box<dyn std::error::Error>> {
        serde_json::to_writer(&mut *writer, &entry)?;
        writeln!(writer)?;
        Ok(())
    };

    // ── System prompt ──
    writeln_json(&mut writer, serde_json::json!({
        "role": "system",
        "content": system_prompt,
        "_meta": {
            "model": model,
            "timestamp": chrono_now(),
        }
    }))?;

    // ── User message ──
    writeln_json(&mut writer, serde_json::json!({
        "role": "user",
        "content": user_message,
    }))?;

    // ── Agent turns ──
    // Track pending tool_call_ids so we can pair ToolExecutionCompleted
    // with the correct call (supports parallel tool calls).
    let mut pending_tool_ids: std::collections::VecDeque<(String, String)> = std::collections::VecDeque::new();

    for (event, usage) in events {
        match event {
            StreamEvent::AssistantTurnComplete(tc) => {
                // Build content string from text blocks
                let content: String = tc.message.content.iter().filter_map(|b| {
                    match b {
                        ContentBlock::Text(t) if !t.text.is_empty() => Some(t.text.as_str()),
                        _ => None,
                    }
                }).collect::<Vec<_>>().join("");

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
                let tool_call_id = pending_tool_ids.iter()
                    .position(|(name, _)| name == &tc.tool_name)
                    .and_then(|i| pending_tool_ids.remove(i))
                    .map(|(_, id)| id)
                    .unwrap_or_default();

                writeln_json(&mut writer, serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "name": tc.tool_name,
                    "content": tc.output,
                    "_meta": {
                        "is_error": tc.is_error,
                        "_t_ms": start.elapsed().as_millis() as u64,
                    }
                }))?;
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
