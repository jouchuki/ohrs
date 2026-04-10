//! Safe Rust trait that plugin authors implement.
//!
//! The `oh-plugin-derive` proc-macro generates the `extern "C"` glue from this trait.

use serde::{Deserialize, Serialize};

/// Plugin manifest returned by the safe trait.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default = "default_true")]
    pub enabled_by_default: bool,
}

fn default_true() -> bool {
    true
}

/// A skill definition for the safe trait.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDef {
    pub name: String,
    pub description: String,
    pub content: String,
}

/// A hook definition for the safe trait.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDef {
    pub event: String,
    pub config: serde_json::Value,
}

/// Result from a plugin command execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub output: String,
    pub is_error: bool,
}

/// The trait that Rust plugin authors implement.
///
/// Use `#[openharness_plugin]` from `oh-plugin-derive` to generate the FFI glue.
pub trait OpenHarnessPlugin: Send + Sync {
    /// Return the plugin manifest.
    fn manifest(&self) -> PluginManifest;

    /// Initialize the plugin with host-provided configuration.
    fn init(&mut self, config: serde_json::Value) -> Result<(), String>;

    /// Return skills provided by this plugin.
    fn skills(&self) -> Vec<SkillDef> {
        vec![]
    }

    /// Return hooks provided by this plugin.
    fn hooks(&self) -> Vec<HookDef> {
        vec![]
    }

    /// Return MCP server configurations as JSON.
    fn mcp_configs(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// Execute a named command.
    fn execute_command(
        &self,
        _command: &str,
        _args: serde_json::Value,
    ) -> Result<CommandResult, String> {
        Err("no commands".into())
    }

    /// Teardown hook called before the plugin is unloaded.
    fn shutdown(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Serde roundtrip tests ---

    #[test]
    fn test_plugin_manifest_serde_roundtrip() {
        let manifest = PluginManifest {
            name: "test-plugin".into(),
            version: "0.1.0".into(),
            description: "A test plugin".into(),
            enabled_by_default: true,
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let recovered: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.name, "test-plugin");
        assert_eq!(recovered.version, "0.1.0");
        assert_eq!(recovered.description, "A test plugin");
        assert!(recovered.enabled_by_default);
    }

    #[test]
    fn test_plugin_manifest_enabled_by_default_defaults_true() {
        let json = r#"{"name":"p","version":"1","description":"d"}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(manifest.enabled_by_default);
    }

    #[test]
    fn test_plugin_manifest_enabled_by_default_false() {
        let json = r#"{"name":"p","version":"1","description":"d","enabled_by_default":false}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert!(!manifest.enabled_by_default);
    }

    #[test]
    fn test_skill_def_serde_roundtrip() {
        let skill = SkillDef {
            name: "greet".into(),
            description: "Says hello".into(),
            content: "Hello, world!".into(),
        };
        let json = serde_json::to_string(&skill).unwrap();
        let recovered: SkillDef = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.name, "greet");
        assert_eq!(recovered.description, "Says hello");
        assert_eq!(recovered.content, "Hello, world!");
    }

    #[test]
    fn test_hook_def_serde_roundtrip() {
        let hook = HookDef {
            event: "on_start".into(),
            config: serde_json::json!({"key": "value"}),
        };
        let json = serde_json::to_string(&hook).unwrap();
        let recovered: HookDef = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.event, "on_start");
        assert_eq!(recovered.config["key"], "value");
    }

    #[test]
    fn test_command_result_serde_roundtrip() {
        let result = CommandResult {
            output: "done".into(),
            is_error: false,
        };
        let json = serde_json::to_string(&result).unwrap();
        let recovered: CommandResult = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.output, "done");
        assert!(!recovered.is_error);
    }

    #[test]
    fn test_command_result_error() {
        let result = CommandResult {
            output: "fail".into(),
            is_error: true,
        };
        let json = serde_json::to_string(&result).unwrap();
        let recovered: CommandResult = serde_json::from_str(&json).unwrap();
        assert!(recovered.is_error);
    }

    // --- Default trait method tests ---

    struct TestPlugin;

    impl OpenHarnessPlugin for TestPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                name: "test".into(),
                version: "0.0.1".into(),
                description: "test plugin".into(),
                enabled_by_default: true,
            }
        }

        fn init(&mut self, _config: serde_json::Value) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn test_default_skills_returns_empty() {
        let plugin = TestPlugin;
        assert!(plugin.skills().is_empty());
    }

    #[test]
    fn test_default_hooks_returns_empty() {
        let plugin = TestPlugin;
        assert!(plugin.hooks().is_empty());
    }

    #[test]
    fn test_default_mcp_configs_returns_null() {
        let plugin = TestPlugin;
        assert_eq!(plugin.mcp_configs(), serde_json::Value::Null);
    }

    #[test]
    fn test_default_execute_command_returns_err() {
        let plugin = TestPlugin;
        let result = plugin.execute_command("anything", serde_json::Value::Null);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "no commands");
    }

    #[test]
    fn test_default_shutdown_does_not_panic() {
        let mut plugin = TestPlugin;
        plugin.shutdown(); // should not panic
    }
}
