//! Plugin manifest and loaded plugin types.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::hooks::HookDefinition;
use crate::mcp::McpServerConfig;
use crate::skills::SkillDefinition;

/// Plugin manifest stored in plugin.json or .claude-plugin/plugin.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_true")]
    pub enabled_by_default: bool,
    #[serde(default = "default_skills_dir")]
    pub skills_dir: String,
    #[serde(default = "default_hooks_file")]
    pub hooks_file: String,
    #[serde(default = "default_mcp_file")]
    pub mcp_file: String,
    pub author: Option<serde_json::Value>,
    pub commands: Option<serde_json::Value>,
    pub agents: Option<serde_json::Value>,
    pub skills: Option<serde_json::Value>,
    pub hooks: Option<serde_json::Value>,
}

fn default_version() -> String {
    "0.0.0".into()
}

fn default_true() -> bool {
    true
}

fn default_skills_dir() -> String {
    "skills".into()
}

fn default_hooks_file() -> String {
    "hooks.json".into()
}

fn default_mcp_file() -> String {
    "mcp.json".into()
}

/// How a plugin was loaded.
#[derive(Debug, Clone)]
pub enum PluginKind {
    /// JSON manifest + markdown skills + hooks.json. No native code.
    Static,
    /// Loaded from a .so/.dll via libloading.
    Native,
}

/// A loaded plugin and its contributed artifacts.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub path: PathBuf,
    pub enabled: bool,
    pub skills: Vec<SkillDefinition>,
    pub hooks: HashMap<String, Vec<HookDefinition>>,
    pub mcp_servers: HashMap<String, McpServerConfig>,
    pub commands: Vec<SkillDefinition>,
    pub kind: PluginKind,
}

impl LoadedPlugin {
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    pub fn description(&self) -> &str {
        &self.manifest.description
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_manifest_serde_roundtrip() {
        let manifest = PluginManifest {
            name: "my-plugin".into(),
            version: "1.0.0".into(),
            description: "A test plugin".into(),
            enabled_by_default: true,
            skills_dir: "skills".into(),
            hooks_file: "hooks.json".into(),
            mcp_file: "mcp.json".into(),
            author: None,
            commands: None,
            agents: None,
            skills: None,
            hooks: None,
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let deser: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.name, "my-plugin");
        assert_eq!(deser.version, "1.0.0");
        assert!(deser.enabled_by_default);
    }

    #[test]
    fn test_plugin_manifest_deserialize_defaults() {
        let json = r#"{"name":"p"}"#;
        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.version, "0.0.0");
        assert!(manifest.description.is_empty());
        assert!(manifest.enabled_by_default);
        assert_eq!(manifest.skills_dir, "skills");
        assert_eq!(manifest.hooks_file, "hooks.json");
        assert_eq!(manifest.mcp_file, "mcp.json");
    }

    #[test]
    fn test_loaded_plugin_accessors() {
        let plugin = LoadedPlugin {
            manifest: PluginManifest {
                name: "test-plugin".into(),
                version: "0.1.0".into(),
                description: "Test".into(),
                enabled_by_default: true,
                skills_dir: "skills".into(),
                hooks_file: "hooks.json".into(),
                mcp_file: "mcp.json".into(),
                author: None,
                commands: None,
                agents: None,
                skills: None,
                hooks: None,
            },
            path: PathBuf::from("/plugins/test"),
            enabled: true,
            skills: vec![],
            hooks: HashMap::new(),
            mcp_servers: HashMap::new(),
            commands: vec![],
            kind: PluginKind::Static,
        };
        assert_eq!(plugin.name(), "test-plugin");
        assert_eq!(plugin.description(), "Test");
    }
}
