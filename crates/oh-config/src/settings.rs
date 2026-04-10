//! Settings model and loading logic for OpenHarness.
//!
//! Precedence (highest first):
//! 1. CLI arguments
//! 2. Environment variables
//! 3. Config file (~/.openharnessrs/settings.json)
//! 4. Defaults

use oh_types::hooks::HookDefinition;
use oh_types::mcp::McpServerConfig;
use oh_types::permissions::PermissionMode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A glob-pattern path permission rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathRuleConfig {
    pub pattern: String,
    #[serde(default = "default_true")]
    pub allow: bool,
}

fn default_true() -> bool {
    true
}

/// Permission mode configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionSettings {
    #[serde(default)]
    pub mode: PermissionMode,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub denied_tools: Vec<String>,
    #[serde(default)]
    pub path_rules: Vec<PathRuleConfig>,
    #[serde(default)]
    pub denied_commands: Vec<String>,
}

impl Default for PermissionSettings {
    fn default() -> Self {
        Self {
            mode: PermissionMode::Default,
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            path_rules: Vec::new(),
            denied_commands: Vec::new(),
        }
    }
}

/// Memory system configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_max_files")]
    pub max_files: u32,
    #[serde(default = "default_max_entrypoint_lines")]
    pub max_entrypoint_lines: u32,
}

fn default_max_files() -> u32 {
    5
}

fn default_max_entrypoint_lines() -> u32 {
    200
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_files: 5,
            max_entrypoint_lines: 200,
        }
    }
}

/// Main settings model for OpenHarness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub api_key: String,
    /// LLM provider: "anthropic" (default) or "openai".
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    pub base_url: Option<String>,
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub permission: PermissionSettings,
    #[serde(default)]
    pub hooks: HashMap<String, Vec<HookDefinition>>,
    #[serde(default)]
    pub memory: MemorySettings,
    #[serde(default)]
    pub enabled_plugins: HashMap<String, bool>,
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_output_style")]
    pub output_style: String,
    #[serde(default)]
    pub vim_mode: bool,
    #[serde(default)]
    pub voice_mode: bool,
    #[serde(default)]
    pub fast_mode: bool,
    #[serde(default = "default_effort")]
    pub effort: String,
    #[serde(default = "default_passes")]
    pub passes: u32,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default)]
    pub verbose: bool,
    /// When true, the Config tool cannot write settings to disk.
    #[serde(default)]
    pub config_readonly: bool,
}

fn default_max_turns() -> u32 {
    30
}

fn default_provider() -> String {
    "anthropic".into()
}

fn default_model() -> String {
    "claude-sonnet-4-6".into()
}

fn default_max_tokens() -> u32 {
    16384
}

fn default_theme() -> String {
    "default".into()
}

fn default_output_style() -> String {
    "default".into()
}

fn default_effort() -> String {
    "medium".into()
}

fn default_passes() -> u32 {
    1
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            provider: default_provider(),
            model: default_model(),
            max_tokens: default_max_tokens(),
            base_url: None,
            system_prompt: None,
            permission: PermissionSettings::default(),
            hooks: HashMap::new(),
            memory: MemorySettings::default(),
            enabled_plugins: HashMap::new(),
            mcp_servers: HashMap::new(),
            theme: default_theme(),
            output_style: default_output_style(),
            vim_mode: false,
            voice_mode: false,
            fast_mode: false,
            effort: default_effort(),
            passes: default_passes(),
            max_turns: default_max_turns(),
            verbose: false,
            config_readonly: false,
        }
    }
}

impl Settings {
    /// Resolve API key: instance value > provider-specific env var > generic env var > error.
    pub fn resolve_api_key(&self) -> Result<String, SettingsError> {
        if !self.api_key.is_empty() {
            return Ok(self.api_key.clone());
        }
        // Try provider-specific env var first
        let env_var = match self.provider.as_str() {
            "openai" => "OPENAI_API_KEY",
            _ => "ANTHROPIC_API_KEY",
        };
        if let Ok(key) = std::env::var(env_var) {
            if !key.is_empty() {
                return Ok(key);
            }
        }
        Err(SettingsError::MissingApiKey)
    }

    /// Returns true if the provider is OpenAI.
    pub fn is_openai(&self) -> bool {
        self.provider == "openai"
            || self
                .base_url
                .as_deref()
                .map(|u| u.contains("openai"))
                .unwrap_or(false)
            || self.model.starts_with("gpt-")
            || self.model.starts_with("o1")
            || self.model.starts_with("o3")
            || self.model.starts_with("o4")
    }

    /// Default model for the configured provider.
    pub fn default_model_for_provider(&self) -> String {
        if self.is_openai() {
            "gpt-5.4-2026-03-05".into()
        } else {
            "claude-sonnet-4-6".into()
        }
    }

    /// Return a new Settings with overrides applied (non-None values only).
    pub fn merge_cli_overrides(&self, overrides: CliOverrides) -> Self {
        let mut s = self.clone();
        if let Some(model) = overrides.model {
            s.model = model;
        }
        if let Some(max_tokens) = overrides.max_tokens {
            s.max_tokens = max_tokens;
        }
        if let Some(base_url) = overrides.base_url {
            s.base_url = Some(base_url);
        }
        if let Some(system_prompt) = overrides.system_prompt {
            s.system_prompt = Some(system_prompt);
        }
        if let Some(api_key) = overrides.api_key {
            s.api_key = api_key;
        }
        s
    }
}

/// CLI override values.
#[derive(Debug, Default)]
pub struct CliOverrides {
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub base_url: Option<String>,
    pub system_prompt: Option<String>,
    pub api_key: Option<String>,
}

/// Helper to read an env var with a primary and fallback name.
fn env_with_fallback(primary: &str, fallback: &str) -> Option<String> {
    std::env::var(primary)
        .ok()
        .or_else(|| std::env::var(fallback).ok())
}

/// Apply environment variable overrides.
pub fn apply_env_overrides(settings: Settings) -> Settings {
    let mut s = settings;

    // Provider override
    if let Some(provider) = env_with_fallback("OPENHARNESSRS_PROVIDER", "OPENHARNESS_PROVIDER") {
        s.provider = provider;
    }

    if let Ok(model) = std::env::var("ANTHROPIC_MODEL") {
        s.model = model;
    } else if let Some(model) = env_with_fallback("OPENHARNESSRS_MODEL", "OPENHARNESS_MODEL") {
        s.model = model;
    }

    // If provider changed to openai but model is still the anthropic default, swap it
    if s.is_openai() && s.model == "claude-sonnet-4-6" {
        s.model = s.default_model_for_provider();
    }

    if let Ok(base_url) = std::env::var("ANTHROPIC_BASE_URL") {
        s.base_url = Some(base_url);
    } else if let Some(base_url) =
        env_with_fallback("OPENHARNESSRS_BASE_URL", "OPENHARNESS_BASE_URL")
    {
        s.base_url = Some(base_url);
    }

    if let Some(max_tokens) =
        env_with_fallback("OPENHARNESSRS_MAX_TOKENS", "OPENHARNESS_MAX_TOKENS")
    {
        if let Ok(n) = max_tokens.parse() {
            s.max_tokens = n;
        }
    }

    // Pick the right API key env var based on provider
    let api_key_var = if s.is_openai() {
        "OPENAI_API_KEY"
    } else {
        "ANTHROPIC_API_KEY"
    };
    if let Ok(api_key) = std::env::var(api_key_var) {
        if !api_key.is_empty() {
            s.api_key = api_key;
        }
    }

    // Permission mode override
    if let Some(mode_str) =
        env_with_fallback("OPENHARNESSRS_PERMISSION_MODE", "OPENHARNESS_PERMISSION_MODE")
    {
        match mode_str.to_lowercase().as_str() {
            "default" => s.permission.mode = PermissionMode::Default,
            "plan" => s.permission.mode = PermissionMode::Plan,
            "full_auto" => s.permission.mode = PermissionMode::FullAuto,
            _ => {} // ignore unknown values
        }
    }

    // Denied tools override
    if let Some(denied) =
        env_with_fallback("OPENHARNESSRS_DENIED_TOOLS", "OPENHARNESS_DENIED_TOOLS")
    {
        s.permission.denied_tools = denied
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
    }

    // Allowed tools override
    if let Some(allowed) =
        env_with_fallback("OPENHARNESSRS_ALLOWED_TOOLS", "OPENHARNESS_ALLOWED_TOOLS")
    {
        s.permission.allowed_tools = allowed
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
    }

    // Hooks override (JSON-encoded)
    if let Some(hooks_json) = env_with_fallback("OPENHARNESSRS_HOOKS", "OPENHARNESS_HOOKS") {
        if let Ok(hooks) = serde_json::from_str(&hooks_json) {
            s.hooks = hooks;
        }
    }

    // Config readonly flag
    if let Some(readonly_str) =
        env_with_fallback("OPENHARNESSRS_CONFIG_READONLY", "OPENHARNESS_CONFIG_READONLY")
    {
        s.config_readonly = matches!(readonly_str.as_str(), "true" | "1");
    }

    s
}

/// Load settings from config file.
///
/// Resolution order for config path:
/// 1. Explicit `config_path` argument
/// 2. `OPENHARNESSRS_CONFIG` env var (e.g. mounted ConfigMap)
/// 3. `OPENHARNESS_CONFIG` env var (legacy fallback)
/// 4. Default path via `get_config_file_path()`
pub fn load_settings(config_path: Option<&Path>) -> Result<Settings, SettingsError> {
    let (path, from_env) = match config_path {
        Some(p) => (p.to_path_buf(), false),
        None => {
            if let Some(env_path) =
                env_with_fallback("OPENHARNESSRS_CONFIG", "OPENHARNESS_CONFIG")
            {
                (PathBuf::from(env_path), true)
            } else {
                (super::paths::get_config_file_path(), false)
            }
        }
    };

    let mut settings = if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| SettingsError::IoError(e.to_string()))?;
        serde_json::from_str(&raw)
            .map_err(|e| SettingsError::ParseError(e.to_string()))?
    } else {
        Settings::default()
    };

    // Mark read-only when loaded from env-var-specified config path
    if from_env {
        settings.config_readonly = true;
    }

    Ok(apply_env_overrides(settings))
}

/// Save settings to config file.
pub fn save_settings(settings: &Settings, config_path: Option<&Path>) -> Result<(), SettingsError> {
    let path = match config_path {
        Some(p) => p.to_path_buf(),
        None => super::paths::get_config_file_path(),
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| SettingsError::IoError(e.to_string()))?;
    }

    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| SettingsError::ParseError(e.to_string()))?;
    std::fs::write(&path, format!("{json}\n"))
        .map_err(|e| SettingsError::IoError(e.to_string()))?;

    Ok(())
}

/// Settings errors.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("no API key found — set ANTHROPIC_API_KEY or OPENAI_API_KEY, or configure api_key in settings.json")]
    MissingApiKey,
    #[error("IO error: {0}")]
    IoError(String),
    #[error("parse error: {0}")]
    ParseError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Settings::default() ──────────────────────────────────────

    #[test]
    fn test_default_values() {
        let s = Settings::default();
        assert_eq!(s.api_key, "");
        assert_eq!(s.model, "claude-sonnet-4-6");
        assert_eq!(s.max_tokens, 16384);
        assert!(s.base_url.is_none());
        assert!(s.system_prompt.is_none());
        assert!(!s.fast_mode);
        assert!(!s.vim_mode);
        assert!(!s.voice_mode);
        assert!(!s.verbose);
        assert_eq!(s.effort, "medium");
        assert_eq!(s.passes, 1);
        assert_eq!(s.theme, "default");
        assert_eq!(s.output_style, "default");
    }

    #[test]
    fn test_permission_settings_defaults() {
        let p = PermissionSettings::default();
        assert_eq!(p.mode, PermissionMode::Default);
        assert!(p.allowed_tools.is_empty());
        assert!(p.denied_tools.is_empty());
        assert!(p.path_rules.is_empty());
        assert!(p.denied_commands.is_empty());
    }

    #[test]
    fn test_memory_settings_defaults() {
        let m = MemorySettings::default();
        assert!(m.enabled);
        assert_eq!(m.max_files, 5);
        assert_eq!(m.max_entrypoint_lines, 200);
    }

    // ── resolve_api_key ──────────────────────────────────────────

    #[test]
    fn test_resolve_api_key_from_instance() {
        let s = Settings {
            api_key: "sk-test-123".into(),
            ..Settings::default()
        };
        assert_eq!(s.resolve_api_key().unwrap(), "sk-test-123");
    }

    #[test]
    fn test_resolve_api_key_from_env() {
        // Safety: we use a unique env var name scheme and clean up.
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "sk-env-456") };
        let s = Settings::default();
        let result = s.resolve_api_key();
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
        assert_eq!(result.unwrap(), "sk-env-456");
    }

    #[test]
    fn test_resolve_api_key_instance_takes_precedence_over_env() {
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "sk-env-456") };
        let s = Settings {
            api_key: "sk-instance-789".into(),
            ..Settings::default()
        };
        let result = s.resolve_api_key();
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
        assert_eq!(result.unwrap(), "sk-instance-789");
    }

    #[test]
    fn test_resolve_api_key_missing_returns_error() {
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
        let s = Settings::default();
        let err = s.resolve_api_key().unwrap_err();
        assert!(matches!(err, SettingsError::MissingApiKey));
    }

    // ── merge_cli_overrides ──────────────────────────────────────

    #[test]
    fn test_merge_cli_overrides_applies_values() {
        let s = Settings::default();
        let updated = s.merge_cli_overrides(CliOverrides {
            model: Some("claude-opus-4-20250514".into()),
            max_tokens: Some(8192),
            ..Default::default()
        });
        assert_eq!(updated.model, "claude-opus-4-20250514");
        assert_eq!(updated.max_tokens, 8192);
        // Unset fields keep their defaults.
        assert_eq!(updated.api_key, "");
    }

    #[test]
    fn test_merge_cli_overrides_none_preserves_original() {
        let s = Settings {
            model: "original-model".into(),
            ..Settings::default()
        };
        let updated = s.merge_cli_overrides(CliOverrides::default());
        assert_eq!(updated.model, "original-model");
    }

    #[test]
    fn test_merge_cli_overrides_returns_new_instance() {
        let s = Settings::default();
        let updated = s.merge_cli_overrides(CliOverrides {
            model: Some("new-model".into()),
            ..Default::default()
        });
        // The original should be unchanged.
        assert_eq!(s.model, "claude-sonnet-4-6");
        assert_eq!(updated.model, "new-model");
    }

    // ── load_settings / save_settings ────────────────────────────

    #[test]
    fn test_load_missing_file_returns_defaults() {
        // Clear env vars that would pollute defaults.
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("ANTHROPIC_MODEL");
            std::env::remove_var("OPENHARNESSRS_MODEL");
            std::env::remove_var("ANTHROPIC_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_MAX_TOKENS");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let s = load_settings(Some(&path)).unwrap();
        assert_eq!(s.model, "claude-sonnet-4-6");
        assert_eq!(s.api_key, "");
    }

    #[test]
    fn test_load_existing_file() {
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("ANTHROPIC_MODEL");
            std::env::remove_var("OPENHARNESSRS_MODEL");
            std::env::remove_var("ANTHROPIC_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_MAX_TOKENS");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"model": "claude-opus-4-20250514", "verbose": true, "fast_mode": true}"#,
        )
        .unwrap();
        let s = load_settings(Some(&path)).unwrap();
        assert_eq!(s.model, "claude-opus-4-20250514");
        assert!(s.verbose);
        assert!(s.fast_mode);
        assert_eq!(s.api_key, ""); // default preserved
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("ANTHROPIC_MODEL");
            std::env::remove_var("OPENHARNESSRS_MODEL");
            std::env::remove_var("ANTHROPIC_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_MAX_TOKENS");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = Settings {
            api_key: "sk-roundtrip".into(),
            model: "claude-opus-4-20250514".into(),
            verbose: true,
            ..Settings::default()
        };
        save_settings(&original, Some(&path)).unwrap();
        let loaded = load_settings(Some(&path)).unwrap();
        assert_eq!(loaded.api_key, original.api_key);
        assert_eq!(loaded.model, original.model);
        assert_eq!(loaded.verbose, original.verbose);
    }

    #[test]
    fn test_save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep").join("nested").join("settings.json");
        save_settings(&Settings::default(), Some(&path)).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_load_with_permission_settings() {
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("ANTHROPIC_MODEL");
            std::env::remove_var("OPENHARNESSRS_MODEL");
            std::env::remove_var("ANTHROPIC_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_MAX_TOKENS");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"permission": {"mode": "full_auto", "allowed_tools": ["Bash", "Read"]}}"#,
        )
        .unwrap();
        let s = load_settings(Some(&path)).unwrap();
        assert_eq!(s.permission.mode, PermissionMode::FullAuto);
        assert_eq!(s.permission.allowed_tools, vec!["Bash", "Read"]);
    }

    // ── apply_env_overrides ──────────────────────────────────────

    #[test]
    fn test_apply_env_overrides_anthropic_model() {
        unsafe {
            std::env::remove_var("OPENHARNESSRS_MODEL");
            std::env::remove_var("ANTHROPIC_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_MAX_TOKENS");
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::set_var("ANTHROPIC_MODEL", "from-env-model");
        }
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("ANTHROPIC_MODEL") };
        assert_eq!(s.model, "from-env-model");
    }

    #[test]
    fn test_apply_env_overrides_anthropic_base_url() {
        unsafe {
            std::env::remove_var("ANTHROPIC_MODEL");
            std::env::remove_var("OPENHARNESSRS_MODEL");
            std::env::remove_var("OPENHARNESSRS_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_MAX_TOKENS");
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::set_var("ANTHROPIC_BASE_URL", "https://env.example/anthropic");
        }
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("ANTHROPIC_BASE_URL") };
        assert_eq!(s.base_url.as_deref(), Some("https://env.example/anthropic"));
    }

    #[test]
    fn test_apply_env_overrides_api_key() {
        unsafe {
            std::env::remove_var("ANTHROPIC_MODEL");
            std::env::remove_var("OPENHARNESSRS_MODEL");
            std::env::remove_var("ANTHROPIC_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_MAX_TOKENS");
            std::env::set_var("ANTHROPIC_API_KEY", "sk-env-override");
        }
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
        assert_eq!(s.api_key, "sk-env-override");
    }

    #[test]
    fn test_apply_env_overrides_max_tokens() {
        unsafe {
            std::env::remove_var("ANTHROPIC_MODEL");
            std::env::remove_var("OPENHARNESSRS_MODEL");
            std::env::remove_var("ANTHROPIC_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_BASE_URL");
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::set_var("OPENHARNESSRS_MAX_TOKENS", "4096");
        }
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_MAX_TOKENS") };
        assert_eq!(s.max_tokens, 4096);
    }

    // ── config_readonly default ───────────────────────────────────

    #[test]
    fn test_config_readonly_defaults_to_false() {
        let s = Settings::default();
        assert!(!s.config_readonly);
    }

    // ── OPENHARNESSRS_CONFIG env var ─────────────────────────────

    fn clear_all_env_vars() {
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("ANTHROPIC_MODEL");
            std::env::remove_var("ANTHROPIC_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_MODEL");
            std::env::remove_var("OPENHARNESSRS_BASE_URL");
            std::env::remove_var("OPENHARNESSRS_MAX_TOKENS");
            std::env::remove_var("OPENHARNESSRS_CONFIG");
            std::env::remove_var("OPENHARNESSRS_PERMISSION_MODE");
            std::env::remove_var("OPENHARNESSRS_DENIED_TOOLS");
            std::env::remove_var("OPENHARNESSRS_ALLOWED_TOOLS");
            std::env::remove_var("OPENHARNESSRS_HOOKS");
            std::env::remove_var("OPENHARNESSRS_CONFIG_READONLY");
            std::env::remove_var("OPENHARNESS_CONFIG");
            std::env::remove_var("OPENHARNESS_MODEL");
            std::env::remove_var("OPENHARNESS_BASE_URL");
            std::env::remove_var("OPENHARNESS_MAX_TOKENS");
            std::env::remove_var("OPENHARNESS_PERMISSION_MODE");
            std::env::remove_var("OPENHARNESS_DENIED_TOOLS");
            std::env::remove_var("OPENHARNESS_ALLOWED_TOOLS");
            std::env::remove_var("OPENHARNESS_HOOKS");
            std::env::remove_var("OPENHARNESS_CONFIG_READONLY");
        }
    }

    #[test]
    fn test_load_settings_from_openharnessrs_config_env() {
        clear_all_env_vars();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mounted.json");
        std::fs::write(&path, r#"{"model": "mounted-model"}"#).unwrap();
        unsafe { std::env::set_var("OPENHARNESSRS_CONFIG", path.to_str().unwrap()) };
        let s = load_settings(None).unwrap();
        unsafe { std::env::remove_var("OPENHARNESSRS_CONFIG") };
        assert_eq!(s.model, "mounted-model");
        assert!(s.config_readonly);
    }

    #[test]
    fn test_load_settings_from_openharness_config_env_fallback() {
        clear_all_env_vars();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy_mounted.json");
        std::fs::write(&path, r#"{"model": "legacy-mounted"}"#).unwrap();
        unsafe { std::env::set_var("OPENHARNESS_CONFIG", path.to_str().unwrap()) };
        let s = load_settings(None).unwrap();
        unsafe { std::env::remove_var("OPENHARNESS_CONFIG") };
        assert_eq!(s.model, "legacy-mounted");
        assert!(s.config_readonly);
    }

    #[test]
    fn test_openharnessrs_config_takes_precedence_over_legacy() {
        clear_all_env_vars();
        let dir = tempfile::tempdir().unwrap();
        let primary_path = dir.path().join("primary.json");
        let legacy_path = dir.path().join("legacy.json");
        std::fs::write(&primary_path, r#"{"model": "primary"}"#).unwrap();
        std::fs::write(&legacy_path, r#"{"model": "legacy"}"#).unwrap();
        unsafe {
            std::env::set_var("OPENHARNESSRS_CONFIG", primary_path.to_str().unwrap());
            std::env::set_var("OPENHARNESS_CONFIG", legacy_path.to_str().unwrap());
        }
        let s = load_settings(None).unwrap();
        unsafe {
            std::env::remove_var("OPENHARNESSRS_CONFIG");
            std::env::remove_var("OPENHARNESS_CONFIG");
        }
        assert_eq!(s.model, "primary");
    }

    #[test]
    fn test_explicit_path_overrides_env_config() {
        clear_all_env_vars();
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("env.json");
        let explicit_path = dir.path().join("explicit.json");
        std::fs::write(&env_path, r#"{"model": "env-model"}"#).unwrap();
        std::fs::write(&explicit_path, r#"{"model": "explicit-model"}"#).unwrap();
        unsafe { std::env::set_var("OPENHARNESSRS_CONFIG", env_path.to_str().unwrap()) };
        let s = load_settings(Some(&explicit_path)).unwrap();
        unsafe { std::env::remove_var("OPENHARNESSRS_CONFIG") };
        assert_eq!(s.model, "explicit-model");
        assert!(!s.config_readonly);
    }

    // ── permission mode env override ────────────────────────────

    #[test]
    fn test_env_override_permission_mode_full_auto() {
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_PERMISSION_MODE", "full_auto") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_PERMISSION_MODE") };
        assert_eq!(s.permission.mode, PermissionMode::FullAuto);
    }

    #[test]
    fn test_env_override_permission_mode_plan() {
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_PERMISSION_MODE", "plan") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_PERMISSION_MODE") };
        assert_eq!(s.permission.mode, PermissionMode::Plan);
    }

    #[test]
    fn test_env_override_permission_mode_legacy_fallback() {
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESS_PERMISSION_MODE", "full_auto") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESS_PERMISSION_MODE") };
        assert_eq!(s.permission.mode, PermissionMode::FullAuto);
    }

    // ── denied/allowed tools env override ───────────────────────

    #[test]
    fn test_env_override_denied_tools() {
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_DENIED_TOOLS", "Bash,Write,Edit") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_DENIED_TOOLS") };
        assert_eq!(s.permission.denied_tools, vec!["Bash", "Write", "Edit"]);
    }

    #[test]
    fn test_env_override_allowed_tools() {
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_ALLOWED_TOOLS", "Read, Grep") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_ALLOWED_TOOLS") };
        assert_eq!(s.permission.allowed_tools, vec!["Read", "Grep"]);
    }

    #[test]
    fn test_env_override_denied_tools_legacy_fallback() {
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESS_DENIED_TOOLS", "Bash") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESS_DENIED_TOOLS") };
        assert_eq!(s.permission.denied_tools, vec!["Bash"]);
    }

    // ── hooks env override ──────────────────────────────────────

    #[test]
    fn test_env_override_hooks() {
        clear_all_env_vars();
        let hooks_json = r#"{"PreToolUse": [{"type": "command", "command": "echo pre"}]}"#;
        unsafe { std::env::set_var("OPENHARNESSRS_HOOKS", hooks_json) };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_HOOKS") };
        assert!(s.hooks.contains_key("PreToolUse"));
        assert_eq!(s.hooks["PreToolUse"].len(), 1);
    }

    #[test]
    fn test_env_override_hooks_invalid_json_ignored() {
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_HOOKS", "not valid json") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_HOOKS") };
        assert!(s.hooks.is_empty());
    }

    // ── config_readonly env override ────────────────────────────

    #[test]
    fn test_env_override_config_readonly_true() {
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_CONFIG_READONLY", "true") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_CONFIG_READONLY") };
        assert!(s.config_readonly);
    }

    #[test]
    fn test_env_override_config_readonly_1() {
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_CONFIG_READONLY", "1") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_CONFIG_READONLY") };
        assert!(s.config_readonly);
    }

    #[test]
    fn test_env_override_config_readonly_false() {
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_CONFIG_READONLY", "false") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_CONFIG_READONLY") };
        assert!(!s.config_readonly);
    }

    #[test]
    fn test_env_override_config_readonly_legacy_fallback() {
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESS_CONFIG_READONLY", "true") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESS_CONFIG_READONLY") };
        assert!(s.config_readonly);
    }

    // ── serialization ────────────────────────────────────────────

    #[test]
    fn test_settings_serialization_roundtrip() {
        let original = Settings {
            api_key: "sk-test".into(),
            model: "test-model".into(),
            max_tokens: 999,
            base_url: Some("https://example.com".into()),
            system_prompt: Some("Be helpful".into()),
            vim_mode: true,
            verbose: true,
            ..Settings::default()
        };
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.api_key, original.api_key);
        assert_eq!(deserialized.model, original.model);
        assert_eq!(deserialized.max_tokens, original.max_tokens);
        assert_eq!(deserialized.base_url, original.base_url);
        assert_eq!(deserialized.system_prompt, original.system_prompt);
        assert_eq!(deserialized.vim_mode, original.vim_mode);
        assert_eq!(deserialized.verbose, original.verbose);
    }

    #[test]
    fn test_deserialize_empty_json_gives_defaults() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.model, "claude-sonnet-4-6");
        assert_eq!(s.max_tokens, 16384);
        assert_eq!(s.effort, "medium");
    }

    #[test]
    fn test_load_invalid_json_returns_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not valid json {{{").unwrap();
        let err = load_settings(Some(&path)).unwrap_err();
        assert!(matches!(err, SettingsError::ParseError(_)));
    }
}
