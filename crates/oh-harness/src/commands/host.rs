//! `ohrs host --json-lines` subcommand.
//!
//! Builds the same `QueryEngine` that the TUI uses, then delegates to the
//! JSON-lines IPC host loop.

use std::path::PathBuf;
use std::sync::Arc;

use oh_api::{AnthropicApiClient, CodexApiClient, OpenAiApiClient, StreamingApiClient};
use oh_config::{load_settings, CliOverrides};
use oh_engine::QueryEngine;
use oh_hooks::executor::{HookExecutionContext, HookExecutor};
use oh_hooks::loader::HookRegistry;
use oh_permissions::PermissionChecker;
use oh_tools::create_default_tool_registry;

use crate::host;

/// Run the host in JSON-lines mode.
pub async fn run_json_lines(
    model: Option<String>,
    max_turns: Option<u32>,
    system_prompt: Option<String>,
    permission_mode: Option<String>,
    settings_path: Option<String>,
    dangerously_skip_permissions: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config_path = settings_path.as_deref().map(PathBuf::from);
    let settings = load_settings(config_path.as_ref().map(|p| p.as_path()))?;

    let settings = settings.merge_cli_overrides(CliOverrides {
        model,
        max_tokens: None,
        base_url: None,
        system_prompt: system_prompt.clone(),
        api_key: None,
    });

    // Resolve API key — use an empty placeholder when none is configured so
    // that shutdown-only sessions (and tests without real credentials) still
    // work.  If the key is truly needed (i.e. submit_line is called), the
    // API client will return a descriptive error at that point.
    let api_key = settings.resolve_api_key().unwrap_or_default();

    let api_client: Arc<dyn StreamingApiClient> = if settings.is_codex() {
        let client = CodexApiClient::from_env().map_err(|e| {
            format!("Codex provider requires CODEX_ACCESS_TOKEN and CODEX_REFRESH_TOKEN: {e}")
        })?;
        Arc::new(client)
    } else if settings.is_openai() {
        Arc::new(OpenAiApiClient::new(&api_key, settings.base_url.as_deref()))
    } else {
        Arc::new(AnthropicApiClient::new(&api_key, settings.base_url.as_deref()))
    };

    let mut perm_settings = settings.permission.clone();
    if dangerously_skip_permissions {
        perm_settings.mode = oh_types::permissions::PermissionMode::FullAuto;
    } else if let Some(ref mode) = permission_mode {
        match mode.as_str() {
            "full_auto" | "auto" => perm_settings.mode = oh_types::permissions::PermissionMode::FullAuto,
            "plan" => perm_settings.mode = oh_types::permissions::PermissionMode::Plan,
            _ => {}
        }
    }
    let permission_checker = Arc::new(PermissionChecker::new(perm_settings));

    let mut hook_registry = HookRegistry::new();
    hook_registry.merge_from_map(&settings.hooks);

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let plugins = oh_plugins::load_all_plugins(&cwd, &settings.enabled_plugins);
    for plugin in &plugins {
        if plugin.enabled {
            hook_registry.merge_from_map(&plugin.hooks);
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

    let sys = system_prompt.unwrap_or_else(|| "You are a helpful AI coding assistant.".into());

    let mut skill_registry_map = serde_json::Map::new();
    let mut skill_entries: Vec<oh_tools::skill::SkillEntry> = Vec::new();
    for plugin in &plugins {
        if !plugin.enabled { continue; }
        for skill in &plugin.skills {
            skill_registry_map.insert(skill.name.clone(), serde_json::json!({ "content": skill.content }));
            skill_entries.push(oh_tools::skill::SkillEntry {
                name: skill.name.clone(),
                description: skill.description.clone(),
            });
        }
    }

    let skill_tool = oh_tools::skill::SkillTool::new();
    if !skill_entries.is_empty() {
        skill_tool.set_available_skills(skill_entries);
    }

    let mut tool_registry = create_default_tool_registry();
    tool_registry.register(Box::new(skill_tool));
    let tool_registry = Arc::new(tool_registry);

    let mut engine = QueryEngine::new(
        api_client,
        tool_registry,
        permission_checker,
        cwd,
        settings.model.clone(),
        sys,
        settings.max_tokens,
    );
    engine.set_hook_executor(hook_executor);

    if !skill_registry_map.is_empty() {
        engine.set_tool_metadata("skill_registry".into(), serde_json::Value::Object(skill_registry_map));
    }

    let max_turns = max_turns.unwrap_or(settings.max_turns);
    engine.set_max_turns(max_turns);

    host::run_host(engine).await
}
