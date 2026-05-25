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
    /// Glob patterns that explicitly opt specific paths out of the hardcoded
    /// sensitive-path blocklist.  Use with care — this is an escape hatch for
    /// cases like reading `~/.ssh/known_hosts` in a CI pipeline.
    #[serde(default)]
    pub allow_sensitive_override: Vec<String>,
}

impl Default for PermissionSettings {
    fn default() -> Self {
        Self {
            mode: PermissionMode::Default,
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            path_rules: Vec::new(),
            denied_commands: Vec::new(),
            allow_sensitive_override: Vec::new(),
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

// ── Sandbox ────────────────────────────────────────────────────────────────

/// Which sandbox backend to use.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SandboxBackendKind {
    /// No sandboxing.
    #[default]
    None,
    /// Linux Landlock (kernel-level LSM).
    Landlock,
    /// Docker container isolation.
    Docker,
}

/// OS-level network restrictions passed to sandbox-runtime.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxNetworkSettings {
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    #[serde(default)]
    pub denied_domains: Vec<String>,
}

/// OS-level filesystem restrictions passed to sandbox-runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxFilesystemSettings {
    #[serde(default)]
    pub allow_read: Vec<String>,
    #[serde(default)]
    pub deny_read: Vec<String>,
    #[serde(default = "default_allow_write")]
    pub allow_write: Vec<String>,
    #[serde(default)]
    pub deny_write: Vec<String>,
}

fn default_allow_write() -> Vec<String> {
    vec![".".into()]
}

impl Default for SandboxFilesystemSettings {
    fn default() -> Self {
        Self {
            allow_read: Vec::new(),
            deny_read: Vec::new(),
            allow_write: default_allow_write(),
            deny_write: Vec::new(),
        }
    }
}

/// Docker-specific sandbox configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerSandboxSettings {
    #[serde(default = "default_docker_image")]
    pub image: String,
    #[serde(default = "default_true")]
    pub auto_build_image: bool,
    #[serde(default)]
    pub cpu_limit: f64,
    #[serde(default)]
    pub memory_limit: String,
    #[serde(default)]
    pub extra_mounts: Vec<String>,
    #[serde(default)]
    pub extra_env: HashMap<String, String>,
}

fn default_docker_image() -> String {
    "openharness-sandbox:latest".into()
}

impl Default for DockerSandboxSettings {
    fn default() -> Self {
        Self {
            image: default_docker_image(),
            auto_build_image: true,
            cpu_limit: 0.0,
            memory_limit: String::new(),
            extra_mounts: Vec::new(),
            extra_env: HashMap::new(),
        }
    }
}

/// Sandbox-runtime integration settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub backend: SandboxBackendKind,
    #[serde(default)]
    pub fail_if_unavailable: bool,
    #[serde(default)]
    pub enabled_platforms: Vec<String>,
    #[serde(default)]
    pub docker: DockerSandboxSettings,
    #[serde(default)]
    pub network: SandboxNetworkSettings,
    #[serde(default)]
    pub filesystem: SandboxFilesystemSettings,
}

// ── Provider profiles ──────────────────────────────────────────────────────

/// Named provider workflow configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderProfile {
    /// Unique identifier for the profile (e.g. "claude", "openai").
    pub id: String,
    /// User-facing display name.
    pub name: String,
    /// Base URL for the provider's API.
    pub base_url: String,
    /// Environment variable names to check for auth credentials.
    #[serde(default)]
    pub auth_env_vars: Vec<String>,
    /// List of known model identifiers for this provider.
    #[serde(default)]
    pub models: Vec<String>,
    /// Maximum context window in tokens (provider-specific).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u32>,
    /// Tokens at which auto-compaction is triggered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold_tokens: Option<u32>,
}

/// Return the 8 built-in provider profiles.
pub fn builtin_profiles() -> Vec<ProviderProfile> {
    vec![
        ProviderProfile {
            // id matches the default `Settings.provider` value ("anthropic")
            // so that `effective_context_window()` resolves without configuration.
            id: "anthropic".into(),
            name: "Anthropic Claude API".into(),
            base_url: "https://api.anthropic.com".into(),
            auth_env_vars: vec!["ANTHROPIC_API_KEY".into()],
            models: vec![
                "claude-sonnet-4-6".into(),
                "claude-opus-4-6".into(),
                "claude-haiku-4-5".into(),
            ],
            context_window_tokens: Some(200_000),
            auto_compact_threshold_tokens: Some(160_000),
        },
        ProviderProfile {
            id: "openai".into(),
            name: "OpenAI".into(),
            // Client appends /v1/chat/completions, so base_url must not include /v1.
            base_url: "https://api.openai.com".into(),
            auth_env_vars: vec!["OPENAI_API_KEY".into()],
            models: vec![
                "gpt-5.4".into(),
                "gpt-4o".into(),
                "o3".into(),
            ],
            context_window_tokens: Some(128_000),
            auto_compact_threshold_tokens: Some(100_000),
        },
        ProviderProfile {
            id: "copilot".into(),
            name: "GitHub Copilot".into(),
            // GitHub Copilot API root. Note: Copilot uses /chat/completions (no /v1
            // prefix), so a dedicated Copilot client must NOT append /v1/chat/completions.
            base_url: "https://api.githubcopilot.com".into(),
            auth_env_vars: vec!["GITHUB_TOKEN".into()],
            models: vec!["gpt-5.4".into(), "claude-sonnet-4-6".into()],
            context_window_tokens: Some(128_000),
            auto_compact_threshold_tokens: Some(100_000),
        },
        ProviderProfile {
            id: "moonshot".into(),
            name: "Moonshot (Kimi)".into(),
            // Client appends /v1/chat/completions, so base_url must not include /v1.
            base_url: "https://api.moonshot.cn".into(),
            auth_env_vars: vec!["MOONSHOT_API_KEY".into()],
            models: vec!["kimi-k2.5".into(), "moonshot-v1-128k".into()],
            context_window_tokens: Some(128_000),
            auto_compact_threshold_tokens: Some(100_000),
        },
        ProviderProfile {
            id: "dashscope".into(),
            name: "Alibaba DashScope".into(),
            // Client appends /v1/chat/completions; DashScope OpenAI-compat path
            // is /compatible-mode/v1, so omit the trailing /v1 here.
            base_url: "https://dashscope.aliyuncs.com/compatible-mode".into(),
            auth_env_vars: vec!["DASHSCOPE_API_KEY".into()],
            models: vec!["qwen-max".into(), "qwen-plus".into()],
            context_window_tokens: Some(32_000),
            auto_compact_threshold_tokens: Some(25_000),
        },
        ProviderProfile {
            id: "gemini".into(),
            name: "Google Gemini".into(),
            // OpenAI-compatible Gemini endpoint; client appends /v1/chat/completions.
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai".into(),
            auth_env_vars: vec!["GEMINI_API_KEY".into(), "GOOGLE_API_KEY".into()],
            models: vec!["gemini-2.5-flash".into(), "gemini-2.5-pro".into()],
            context_window_tokens: Some(1_000_000),
            auto_compact_threshold_tokens: Some(800_000),
        },
        ProviderProfile {
            id: "minimax".into(),
            name: "MiniMax".into(),
            // Client appends /v1/chat/completions, so base_url must not include /v1.
            base_url: "https://api.minimax.chat".into(),
            auth_env_vars: vec!["MINIMAX_API_KEY".into()],
            models: vec!["MiniMax-M2.7".into()],
            context_window_tokens: Some(1_000_000),
            auto_compact_threshold_tokens: Some(800_000),
        },
        ProviderProfile {
            id: "bedrock".into(),
            name: "AWS Bedrock".into(),
            // Bedrock endpoints are region-dependent; use placeholder.
            base_url: "https://bedrock-runtime.{region}.amazonaws.com".into(),
            auth_env_vars: vec![
                "AWS_ACCESS_KEY_ID".into(),
                "AWS_SECRET_ACCESS_KEY".into(),
                "AWS_SESSION_TOKEN".into(),
            ],
            models: vec![
                "anthropic.claude-sonnet-4-6-v1:0".into(),
                "anthropic.claude-opus-4-20250514-v1:0".into(),
            ],
            context_window_tokens: Some(200_000),
            auto_compact_threshold_tokens: Some(160_000),
        },
    ]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    /// Sandbox isolation settings.
    #[serde(default)]
    pub sandbox: SandboxSettings,
    /// Provider profiles (user-defined + built-ins via [`builtin_profiles`]).
    #[serde(default)]
    pub provider_profiles: Vec<ProviderProfile>,
    /// Global default context-window size in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u32>,
    /// Global default auto-compaction threshold in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold_tokens: Option<u32>,
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
            sandbox: SandboxSettings::default(),
            provider_profiles: Vec::new(),
            context_window_tokens: None,
            auto_compact_threshold_tokens: None,
        }
    }
}

impl Settings {
    /// Resolve API key: instance value > provider-specific env var > generic env var > error.
    ///
    /// For the Codex ChatGPT OAuth provider (`openai-codex`), no API key is
    /// required — the access/refresh tokens live in env vars consumed by the
    /// provider itself. Callers should check `is_codex()` before calling this.
    pub fn resolve_api_key(&self) -> Result<String, SettingsError> {
        if self.is_codex() {
            // Codex uses OAuth tokens via env vars, not an API key.
            return Ok(String::new());
        }
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

    /// Returns true if the provider is the Codex ChatGPT OAuth backend.
    pub fn is_codex(&self) -> bool {
        matches!(
            self.provider.as_str(),
            "openai-codex" | "codex" | "codex-chatgpt"
        )
    }

    /// Returns true if the provider is OpenAI (regular API key; not Codex).
    pub fn is_openai(&self) -> bool {
        if self.is_codex() {
            return false;
        }
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

    /// Resolve a provider profile by `id`.
    ///
    /// Only searches user-defined profiles stored in `self.provider_profiles`.
    /// Returns `None` when not found there. To look up built-in profiles use
    /// [`builtin_profiles`] directly, as they are returned by value.
    pub fn resolve_profile(&self, id: &str) -> Option<&ProviderProfile> {
        self.provider_profiles.iter().find(|p| p.id == id)
    }

    /// Return the effective context-window size in tokens.
    ///
    /// Resolution order:
    /// 1. Top-level `context_window_tokens` on this `Settings` instance (global default).
    /// 2. The matching built-in profile for the current `provider`.
    /// 3. `None`.
    pub fn effective_context_window(&self) -> Option<u32> {
        if let Some(n) = self.context_window_tokens {
            return Some(n);
        }
        // Fall back to the built-in profile matching the current provider.
        builtin_profiles()
            .into_iter()
            .find(|p| p.id == self.provider)
            .and_then(|p| p.context_window_tokens)
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Safety: we use a unique env var name scheme and clean up.
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "sk-env-456") };
        let s = Settings::default();
        let result = s.resolve_api_key();
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
        assert_eq!(result.unwrap(), "sk-env-456");
    }

    #[test]
    fn test_resolve_api_key_instance_takes_precedence_over_env() {
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_PERMISSION_MODE", "full_auto") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_PERMISSION_MODE") };
        assert_eq!(s.permission.mode, PermissionMode::FullAuto);
    }

    #[test]
    fn test_env_override_permission_mode_plan() {
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_PERMISSION_MODE", "plan") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_PERMISSION_MODE") };
        assert_eq!(s.permission.mode, PermissionMode::Plan);
    }

    #[test]
    fn test_env_override_permission_mode_legacy_fallback() {
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESS_PERMISSION_MODE", "full_auto") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESS_PERMISSION_MODE") };
        assert_eq!(s.permission.mode, PermissionMode::FullAuto);
    }

    // ── denied/allowed tools env override ───────────────────────

    #[test]
    fn test_env_override_denied_tools() {
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_DENIED_TOOLS", "Bash,Write,Edit") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_DENIED_TOOLS") };
        assert_eq!(s.permission.denied_tools, vec!["Bash", "Write", "Edit"]);
    }

    #[test]
    fn test_env_override_allowed_tools() {
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_ALLOWED_TOOLS", "Read, Grep") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_ALLOWED_TOOLS") };
        assert_eq!(s.permission.allowed_tools, vec!["Read", "Grep"]);
    }

    #[test]
    fn test_env_override_denied_tools_legacy_fallback() {
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESS_DENIED_TOOLS", "Bash") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESS_DENIED_TOOLS") };
        assert_eq!(s.permission.denied_tools, vec!["Bash"]);
    }

    // ── hooks env override ──────────────────────────────────────

    #[test]
    fn test_env_override_hooks() {
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_HOOKS", "not valid json") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_HOOKS") };
        assert!(s.hooks.is_empty());
    }

    // ── config_readonly env override ────────────────────────────

    #[test]
    fn test_env_override_config_readonly_true() {
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_CONFIG_READONLY", "true") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_CONFIG_READONLY") };
        assert!(s.config_readonly);
    }

    #[test]
    fn test_env_override_config_readonly_1() {
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_CONFIG_READONLY", "1") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_CONFIG_READONLY") };
        assert!(s.config_readonly);
    }

    #[test]
    fn test_env_override_config_readonly_false() {
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_env_vars();
        unsafe { std::env::set_var("OPENHARNESSRS_CONFIG_READONLY", "false") };
        let s = apply_env_overrides(Settings::default());
        unsafe { std::env::remove_var("OPENHARNESSRS_CONFIG_READONLY") };
        assert!(!s.config_readonly);
    }

    #[test]
    fn test_env_override_config_readonly_legacy_fallback() {
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

    // ── Sandbox settings ─────────────────────────────────────────

    #[test]
    fn test_sandbox_defaults() {
        let s = SandboxSettings::default();
        assert!(!s.enabled);
        assert_eq!(s.backend, SandboxBackendKind::None);
        assert!(!s.fail_if_unavailable);
        assert!(s.enabled_platforms.is_empty());
        assert_eq!(s.docker.image, "openharness-sandbox:latest");
        assert!(s.docker.auto_build_image);
        assert!(s.network.allowed_domains.is_empty());
        assert_eq!(s.filesystem.allow_write, vec!["."]);
    }

    #[test]
    fn test_sandbox_roundtrip_json() {
        let original = SandboxSettings {
            enabled: true,
            backend: SandboxBackendKind::Docker,
            fail_if_unavailable: true,
            enabled_platforms: vec!["linux".into()],
            docker: DockerSandboxSettings {
                image: "my-image:v2".into(),
                memory_limit: "512m".into(),
                extra_mounts: vec!["/host:/container".into()],
                ..DockerSandboxSettings::default()
            },
            network: SandboxNetworkSettings {
                allowed_domains: vec!["api.anthropic.com".into()],
                denied_domains: vec!["evil.example".into()],
            },
            filesystem: SandboxFilesystemSettings {
                allow_read: vec!["/usr/lib".into()],
                deny_write: vec!["/etc".into()],
                ..SandboxFilesystemSettings::default()
            },
        };
        let json = serde_json::to_string_pretty(&original).unwrap();
        let restored: SandboxSettings = serde_json::from_str(&json).unwrap();
        assert!(restored.enabled);
        assert_eq!(restored.backend, SandboxBackendKind::Docker);
        assert!(restored.fail_if_unavailable);
        assert_eq!(restored.docker.image, "my-image:v2");
        assert_eq!(restored.docker.memory_limit, "512m");
        assert_eq!(restored.docker.extra_mounts, vec!["/host:/container"]);
        assert_eq!(restored.network.allowed_domains, vec!["api.anthropic.com"]);
        assert_eq!(restored.network.denied_domains, vec!["evil.example"]);
        assert_eq!(restored.filesystem.allow_read, vec!["/usr/lib"]);
        assert_eq!(restored.filesystem.deny_write, vec!["/etc"]);
    }

    #[test]
    fn test_minimal_config_parses_to_sandbox_defaults() {
        let s: Settings = serde_json::from_str(r#"{"model": "test-model"}"#).unwrap();
        assert!(!s.sandbox.enabled);
        assert_eq!(s.sandbox.backend, SandboxBackendKind::None);
        assert!(s.sandbox.network.allowed_domains.is_empty());
    }

    // ── ProviderProfile / builtin_profiles ──────────────────────

    #[test]
    fn test_builtin_profiles_returns_eight() {
        let profiles = builtin_profiles();
        assert_eq!(profiles.len(), 8, "expected exactly 8 built-in profiles");
    }

    #[test]
    fn test_builtin_profile_ids_are_unique() {
        let profiles = builtin_profiles();
        let mut ids: Vec<&str> = profiles.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        let deduped: Vec<&str> = {
            let mut v = ids.clone();
            v.dedup();
            v
        };
        assert_eq!(ids.len(), deduped.len(), "profile ids must be unique");
    }

    #[test]
    fn test_resolve_profile_moonshot_base_url() {
        // moonshot is a built-in; not in `provider_profiles` by default.
        // Use builtin_profiles() directly as documented.
        let profiles = builtin_profiles();
        let moonshot = profiles.iter().find(|p| p.id == "moonshot").unwrap();
        // Base URL without /v1 suffix; client appends /v1/chat/completions.
        assert_eq!(moonshot.base_url, "https://api.moonshot.cn");
    }

    #[test]
    fn test_resolve_profile_user_defined_takes_precedence() {
        let custom = ProviderProfile {
            id: "moonshot".into(),
            name: "Custom Moonshot".into(),
            base_url: "https://custom.moonshot.example/v1".into(),
            auth_env_vars: vec!["MY_KEY".into()],
            models: vec![],
            context_window_tokens: None,
            auto_compact_threshold_tokens: None,
        };
        let s = Settings {
            provider_profiles: vec![custom],
            ..Settings::default()
        };
        let resolved = s.resolve_profile("moonshot").unwrap();
        assert_eq!(resolved.base_url, "https://custom.moonshot.example/v1");
        assert_eq!(resolved.name, "Custom Moonshot");
    }

    #[test]
    fn test_resolve_profile_missing_returns_none() {
        let s = Settings::default();
        assert!(s.resolve_profile("nonexistent-provider").is_none());
    }

    #[test]
    fn test_provider_profile_roundtrip_json() {
        let original = ProviderProfile {
            id: "test-provider".into(),
            name: "Test Provider".into(),
            base_url: "https://test.example.com/v1".into(),
            auth_env_vars: vec!["TEST_API_KEY".into()],
            models: vec!["model-a".into(), "model-b".into()],
            context_window_tokens: Some(50_000),
            auto_compact_threshold_tokens: Some(40_000),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: ProviderProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.id, original.id);
        assert_eq!(restored.base_url, original.base_url);
        assert_eq!(restored.auth_env_vars, original.auth_env_vars);
        assert_eq!(restored.models, original.models);
        assert_eq!(restored.context_window_tokens, Some(50_000));
        assert_eq!(restored.auto_compact_threshold_tokens, Some(40_000));
    }

    // ── Context-window tokens ────────────────────────────────────

    #[test]
    fn test_context_window_tokens_default_none() {
        let s = Settings::default();
        assert!(s.context_window_tokens.is_none());
        assert!(s.auto_compact_threshold_tokens.is_none());
    }

    #[test]
    fn test_effective_context_window_global_override() {
        let s = Settings {
            context_window_tokens: Some(99_999),
            ..Settings::default()
        };
        assert_eq!(s.effective_context_window(), Some(99_999));
    }

    #[test]
    fn test_effective_context_window_from_builtin_profile() {
        // provider = "anthropic" (the default) should resolve the "anthropic" built-in.
        let s = Settings::default();
        assert_eq!(s.provider, "anthropic");
        assert_eq!(s.effective_context_window(), Some(200_000));
    }

    #[test]
    fn test_effective_context_window_no_match_returns_none() {
        let s = Settings {
            provider: "unknown-provider".into(),
            ..Settings::default()
        };
        assert_eq!(s.effective_context_window(), None);
    }

    #[test]
    fn test_full_settings_with_new_groups_roundtrip() {
        // Build a Settings that exercises all three new groups.
        let original = Settings {
            context_window_tokens: Some(100_000),
            auto_compact_threshold_tokens: Some(80_000),
            sandbox: SandboxSettings {
                enabled: true,
                backend: SandboxBackendKind::Landlock,
                network: SandboxNetworkSettings {
                    allowed_domains: vec!["api.anthropic.com".into()],
                    ..SandboxNetworkSettings::default()
                },
                ..SandboxSettings::default()
            },
            provider_profiles: vec![ProviderProfile {
                id: "custom".into(),
                name: "Custom Provider".into(),
                base_url: "https://custom.api/v1".into(),
                auth_env_vars: vec!["CUSTOM_KEY".into()],
                models: vec!["model-x".into()],
                context_window_tokens: Some(32_000),
                auto_compact_threshold_tokens: Some(25_000),
            }],
            ..Settings::default()
        };

        let json = serde_json::to_string_pretty(&original).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.context_window_tokens, Some(100_000));
        assert_eq!(restored.auto_compact_threshold_tokens, Some(80_000));
        assert!(restored.sandbox.enabled);
        assert_eq!(restored.sandbox.backend, SandboxBackendKind::Landlock);
        assert_eq!(
            restored.sandbox.network.allowed_domains,
            vec!["api.anthropic.com"]
        );
        assert_eq!(restored.provider_profiles.len(), 1);
        assert_eq!(restored.provider_profiles[0].id, "custom");
        assert_eq!(
            restored.provider_profiles[0].base_url,
            "https://custom.api/v1"
        );
        assert_eq!(
            restored.provider_profiles[0].context_window_tokens,
            Some(32_000)
        );
    }

    #[test]
    fn test_minimal_config_new_groups_default() {
        // A completely empty JSON should parse to sane defaults for all new groups.
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(!s.sandbox.enabled);
        assert_eq!(s.sandbox.backend, SandboxBackendKind::None);
        assert!(s.provider_profiles.is_empty());
        assert!(s.context_window_tokens.is_none());
        assert!(s.auto_compact_threshold_tokens.is_none());
    }

    // ── Built-in profile base-URL spot-checks ───────────────────

    #[test]
    fn test_builtin_profile_base_urls() {
        let profiles = builtin_profiles();
        let get = |id: &str| {
            profiles
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.base_url.as_str())
                .unwrap_or("")
                .to_string()
        };
        // "anthropic" matches Settings.provider default so effective_context_window resolves.
        assert_eq!(get("anthropic"), "https://api.anthropic.com");
        // OpenAI-compatible clients append /v1/chat/completions so base_url omits /v1.
        assert_eq!(get("openai"), "https://api.openai.com");
        // Copilot root (uses /chat/completions without /v1 — Copilot client must not append /v1).
        assert_eq!(get("copilot"), "https://api.githubcopilot.com");
        assert_eq!(get("moonshot"), "https://api.moonshot.cn");
        assert_eq!(
            get("dashscope"),
            "https://dashscope.aliyuncs.com/compatible-mode"
        );
        assert_eq!(
            get("gemini"),
            "https://generativelanguage.googleapis.com/v1beta/openai"
        );
        assert_eq!(get("minimax"), "https://api.minimax.chat");
        // Bedrock is region-dependent; just confirm it contains "bedrock-runtime".
        assert!(get("bedrock").contains("bedrock-runtime"));
    }
}
