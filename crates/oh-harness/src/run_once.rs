//! `oh run` — one-shot, non-interactive subagent invocation.
//!
//! Builds a [`QueryContext`](oh_engine::QueryContext) from the same constructors
//! the interactive path uses (API client, tool registry, permission checker,
//! hook executor, system prompt) and calls
//! [`run_subagent`](oh_engine::run_subagent), printing the final assistant text.
//!
//! This is the subcommand that [`SubprocessBackend`](oh_swarm::SubprocessBackend)
//! shells out to: the prompt and options arrive as command-line flags only.

use std::path::PathBuf;
use std::sync::Arc;

use oh_api::{AnthropicApiClient, CodexApiClient, OpenAiApiClient, StreamingApiClient};
use oh_config::{load_settings, CliOverrides};
use oh_engine::{run_subagent, QueryContext};
use oh_hooks::executor::{HookExecutionContext, HookExecutor};
use oh_hooks::loader::HookRegistry;
use oh_hooks::HookExecutorTrait;
use oh_permissions::PermissionChecker;
use oh_tools::create_default_tool_registry;
use oh_types::subagent::AgentId;

/// Flags for the `oh run` one-shot subcommand.
#[derive(clap::Args, Debug)]
pub struct RunArgs {
    /// Prompt to send to the agent.
    #[arg(long)]
    pub prompt: String,

    /// Override the system prompt.
    #[arg(short = 's', long)]
    pub system_prompt: Option<String>,

    /// Model to use.
    #[arg(short = 'm', long)]
    pub model: Option<String>,

    /// Resolve a built-in agent definition by name and apply its system prompt /
    /// tool policy (e.g. `general-purpose`, `Explore`, `Plan`, `worker`).
    #[arg(long)]
    pub agent_def: Option<String>,

    /// Emit the result as `{"result": "...", "agent_id": "..."}` JSON.
    #[arg(long)]
    pub json: bool,

    /// Path to settings file.
    #[arg(long)]
    pub settings: Option<String>,
}

/// Run a single prompt to completion and print the final assistant text.
pub async fn run(args: RunArgs) -> Result<(), Box<dyn std::error::Error>> {
    // --- settings + CLI overrides (mirror cli::run) ---
    let config_path = args.settings.as_deref().map(PathBuf::from);
    let settings = load_settings(config_path.as_deref())?;

    // If an agent definition was named, resolve it now so its model / system
    // prompt can feed the override layer below.
    let agent_def = args
        .agent_def
        .as_deref()
        .map(oh_services::coordinator::agent_definitions::resolve);

    // Model precedence: --model flag > agent-def model > settings default.
    let model_override = args
        .model
        .clone()
        .or_else(|| agent_def.as_ref().and_then(|d| d.model.clone()));

    // System-prompt precedence: --system-prompt flag > agent-def system prompt.
    let system_prompt_override = args
        .system_prompt
        .clone()
        .or_else(|| agent_def.as_ref().and_then(|d| d.system_prompt.clone()));

    let settings = settings.merge_cli_overrides(CliOverrides {
        model: model_override,
        max_tokens: None,
        base_url: None,
        system_prompt: system_prompt_override.clone(),
        api_key: None,
    });

    let api_key = settings.resolve_api_key()?;

    // --- API client — pick provider automatically (mirror cli::run) ---
    let api_client: Arc<dyn StreamingApiClient> = if settings.is_codex() {
        let client = CodexApiClient::from_env().map_err(|e| {
            format!(
                "Codex provider requires CODEX_ACCESS_TOKEN and CODEX_REFRESH_TOKEN env vars: {e}"
            )
        })?;
        Arc::new(client)
    } else if settings.is_openai() {
        Arc::new(OpenAiApiClient::new(&api_key, settings.base_url.as_deref()))
    } else {
        Arc::new(AnthropicApiClient::new(
            &api_key,
            settings.base_url.as_deref(),
        ))
    };

    // --- permission checker ---
    let permission_checker = Arc::new(PermissionChecker::new(settings.permission.clone()));

    // --- cwd (inherit from parent) ---
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // --- hook executor ---
    let mut hook_registry = HookRegistry::new();
    hook_registry.merge_from_map(&settings.hooks);

    let hook_executor_concrete = HookExecutor::new(
        hook_registry,
        HookExecutionContext {
            cwd: cwd.clone(),
            api_client: api_client.clone(),
            default_model: settings.model.clone(),
        },
    );
    // HOOK-1 / C7: keep the live registry handle so HookManage mutations apply.
    let hook_registry_handle = hook_executor_concrete.registry_handle();
    let hook_executor: Arc<dyn HookExecutorTrait> = Arc::new(hook_executor_concrete);

    // ENG-1: proactive compactor, gated on the configured threshold.
    let compactor = settings
        .auto_compact_threshold_tokens
        .map(|threshold| Arc::new(oh_services::compact::Compactor::new(threshold, threshold / 8)));

    // --- system prompt ---
    let system_prompt: String = oh_services::prompts::PromptBuilder::new(&cwd)
        .with_override(system_prompt_override.as_deref())
        .build();

    // --- tool registry, scoped by the agent definition's tool policy ---
    let tool_registry = Arc::new(build_tool_registry(agent_def.as_ref()));

    let agent_id = AgentId::new("oneshot");

    let ctx = QueryContext {
        api_client,
        tool_registry,
        permission_checker,
        cwd,
        model: settings.model.clone(),
        system_prompt,
        max_tokens: settings.max_tokens,
        permission_prompt: None,
        ask_user_prompt: None,
        max_turns: agent_def
            .as_ref()
            .and_then(|d| d.max_turns)
            .unwrap_or(settings.max_turns),
        hook_executor: Some(hook_executor),
        tool_metadata: std::collections::HashMap::new(),
        agent_id: agent_id.clone(),
        parent_id: None,
        session_id: None,
        subagents: None,
        tasks: None,
        cancel: oh_engine::CancellationToken::new(),
        hook_registry: Some(hook_registry_handle),
        compactor,
    };

    let result = run_subagent(ctx, args.prompt).await?;

    if args.json {
        let payload = serde_json::json!({
            "result": result,
            "agent_id": agent_id.as_str(),
        });
        println!("{}", serde_json::to_string(&payload)?);
    } else {
        println!("{result}");
    }

    Ok(())
}

/// Build the default tool registry, then drop tools the agent definition's
/// [`ToolPolicy`](oh_services::coordinator::agent_definitions::ToolPolicy)
/// disallows. With no definition (or `AllowAll`) the full registry is returned.
fn build_tool_registry(
    agent_def: Option<&oh_services::coordinator::agent_definitions::AgentDefinition>,
) -> oh_tools::ToolRegistry {
    use oh_services::coordinator::agent_definitions::ToolPolicy;

    let mut registry = create_default_tool_registry();
    let policy = match agent_def {
        Some(def) => &def.tools,
        None => return registry,
    };

    match policy {
        ToolPolicy::AllowAll => {}
        ToolPolicy::AllowList { list } => {
            registry.retain(|name| list.iter().any(|n| n == name));
        }
        ToolPolicy::DenyList { list } => {
            registry.retain(|name| !list.iter().any(|n| n == name));
        }
    }
    registry
}
