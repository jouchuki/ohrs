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
            save_trajectory(traj_path, &events)?;
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

/// Serialize trajectory as structured action records to a JSONL file.
///
/// Produces a human-readable replay log with entries like:
///   {"turn":1, "action":"reasoning", "text":"Let me examine..."}
///   {"turn":1, "action":"tool_call", "tool":"bash", "input":{...}}
///   {"turn":1, "action":"tool_result", "tool":"bash", "output":"...", "is_error":false}
///   {"turn":2, "action":"reasoning", "text":"Based on the output..."}
fn save_trajectory(
    path: &str,
    events: &[(oh_types::stream_events::StreamEvent, Option<oh_types::api::UsageSnapshot>)],
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use oh_types::stream_events::StreamEvent;
    use oh_types::messages::ContentBlock;

    let file = std::fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    let mut turn: u32 = 0;
    let mut seq: u32 = 0;

    let mut write_entry = |writer: &mut std::io::BufWriter<std::fs::File>,
                           entry: serde_json::Value|
     -> Result<(), Box<dyn std::error::Error>> {
        serde_json::to_writer(&mut *writer, &entry)?;
        writeln!(writer)?;
        Ok(())
    };

    for (event, usage) in events {
        match event {
            StreamEvent::AssistantTurnComplete(tc) => {
                turn += 1;
                // Extract reasoning text and tool calls separately
                let mut reasoning_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<serde_json::Value> = Vec::new();

                for block in &tc.message.content {
                    match block {
                        ContentBlock::Text(t) if !t.text.trim().is_empty() => {
                            reasoning_parts.push(t.text.clone());
                        }
                        ContentBlock::ToolUse(tu) => {
                            tool_calls.push(serde_json::json!({
                                "id": tu.id,
                                "name": tu.name,
                                "input": tu.input,
                            }));
                        }
                        _ => {}
                    }
                }

                // Emit reasoning entry if the model produced any text
                if !reasoning_parts.is_empty() {
                    write_entry(&mut writer, serde_json::json!({
                        "seq": seq,
                        "turn": turn,
                        "action": "reasoning",
                        "text": reasoning_parts.join("\n"),
                        "usage": usage,
                    }))?;
                    seq += 1;
                }

                // Emit each tool call the model decided to make
                for tc in &tool_calls {
                    write_entry(&mut writer, serde_json::json!({
                        "seq": seq,
                        "turn": turn,
                        "action": "tool_call",
                        "tool": tc["name"],
                        "tool_call_id": tc["id"],
                        "input": tc["input"],
                    }))?;
                    seq += 1;
                }
            }
            StreamEvent::ToolExecutionStarted(_) => {
                // Captured in AssistantTurnComplete tool_calls above;
                // skip to avoid duplication.
            }
            StreamEvent::ToolExecutionCompleted(tc) => {
                write_entry(&mut writer, serde_json::json!({
                    "seq": seq,
                    "turn": turn,
                    "action": "tool_result",
                    "tool": tc.tool_name,
                    "output": tc.output,
                    "is_error": tc.is_error,
                }))?;
                seq += 1;
            }
            StreamEvent::AssistantTextDelta(_) => {
                // Streaming fragments — full text captured in TurnComplete.
            }
        }
    }

    // Summary footer
    write_entry(&mut writer, serde_json::json!({
        "seq": seq,
        "action": "trajectory_summary",
        "total_turns": turn,
        "total_events": seq,
    }))?;

    Ok(())
}
