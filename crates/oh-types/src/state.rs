//! Application state models.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Shared mutable UI/session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    pub model: String,
    pub permission_mode: String,
    pub theme: String,
    #[serde(default = "default_cwd")]
    pub cwd: String,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_auth_status")]
    pub auth_status: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub vim_enabled: bool,
    #[serde(default)]
    pub voice_enabled: bool,
    #[serde(default)]
    pub voice_available: bool,
    #[serde(default)]
    pub voice_reason: String,
    #[serde(default)]
    pub fast_mode: bool,
    #[serde(default = "default_effort")]
    pub effort: String,
    #[serde(default = "default_passes")]
    pub passes: u32,
    #[serde(default)]
    pub mcp_connected: u32,
    #[serde(default)]
    pub mcp_failed: u32,
    #[serde(default)]
    pub bridge_sessions: u32,
    #[serde(default = "default_output_style")]
    pub output_style: String,
    #[serde(default)]
    pub keybindings: HashMap<String, String>,
}

fn default_cwd() -> String {
    ".".into()
}

fn default_provider() -> String {
    "unknown".into()
}

fn default_auth_status() -> String {
    "missing".into()
}

fn default_effort() -> String {
    "medium".into()
}

fn default_passes() -> u32 {
    1
}

fn default_output_style() -> String {
    "default".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_state_serde_roundtrip() {
        let state = AppState {
            model: "claude-3".into(),
            permission_mode: "default".into(),
            theme: "dark".into(),
            cwd: "/home/user".into(),
            provider: "anthropic".into(),
            auth_status: "ok".into(),
            base_url: "".into(),
            vim_enabled: false,
            voice_enabled: false,
            voice_available: true,
            voice_reason: "".into(),
            fast_mode: false,
            effort: "medium".into(),
            passes: 1,
            mcp_connected: 2,
            mcp_failed: 0,
            bridge_sessions: 0,
            output_style: "default".into(),
            keybindings: HashMap::new(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let deser: AppState = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.model, "claude-3");
        assert_eq!(deser.mcp_connected, 2);
    }

    #[test]
    fn test_app_state_deserialize_defaults() {
        let json = r#"{"model":"m","permission_mode":"default","theme":"dark"}"#;
        let state: AppState = serde_json::from_str(json).unwrap();
        assert_eq!(state.cwd, ".");
        assert_eq!(state.provider, "unknown");
        assert_eq!(state.auth_status, "missing");
        assert_eq!(state.effort, "medium");
        assert_eq!(state.passes, 1);
        assert_eq!(state.output_style, "default");
    }
}
