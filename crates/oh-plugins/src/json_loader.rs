//! Load JSON-manifest + markdown skill plugins (static plugins).
//! Direct port of Python's plugins/loader.py.

use oh_types::hooks::HookDefinition;
use oh_types::mcp::{McpJsonConfig, McpServerConfig};
use oh_types::plugin::{LoadedPlugin, PluginKind, PluginManifest};
use oh_types::skills::SkillDefinition;
use std::collections::HashMap;
use std::path::Path;

/// Load a static plugin from a directory.
pub fn load_static_plugin(
    path: &Path,
    enabled_plugins: &HashMap<String, bool>,
) -> Result<LoadedPlugin, PluginLoadError> {
    let manifest_path = crate::discovery::find_manifest(path)
        .ok_or_else(|| PluginLoadError::NoManifest(path.display().to_string()))?;

    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| PluginLoadError::Io(e.to_string()))?;

    let manifest: PluginManifest = serde_json::from_str(&manifest_text)
        .map_err(|e| PluginLoadError::Parse(e.to_string()))?;

    let enabled = enabled_plugins
        .get(&manifest.name)
        .copied()
        .unwrap_or(manifest.enabled_by_default);

    // Load skills from skills_dir
    let mut skills = load_skills_from_dir(&path.join(&manifest.skills_dir));

    // Load skills from commands/ directory
    let commands_dir = path.join("commands");
    if commands_dir.exists() {
        skills.extend(load_skills_from_dir(&commands_dir));
    }

    // Load skills from agents/ directory
    let agents_dir = path.join("agents");
    if agents_dir.exists() {
        skills.extend(load_skills_from_dir(&agents_dir));
    }

    // Load hooks
    let hooks = load_hooks_from_file(&path.join(&manifest.hooks_file));

    // Load MCP configs
    let mcp = load_mcp_from_file(&path.join(&manifest.mcp_file));

    let commands: Vec<SkillDefinition> = skills
        .iter()
        .filter(|s| s.source == "plugin")
        .cloned()
        .collect();

    Ok(LoadedPlugin {
        manifest,
        path: path.to_path_buf(),
        enabled,
        skills,
        hooks,
        mcp_servers: mcp,
        commands,
        kind: PluginKind::Static,
    })
}

/// Load markdown skill files from a directory.
fn load_skills_from_dir(dir: &Path) -> Vec<SkillDefinition> {
    if !dir.exists() {
        return Vec::new();
    }

    let mut skills = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s == "md")
                .unwrap_or(false)
        })
        .collect();
    entries.sort_by_key(|e| e.path());

    for entry in entries {
        let path = entry.path();
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        let (name, description) = parse_skill_markdown(stem, &content);
        skills.push(SkillDefinition {
            name,
            description,
            content,
            source: "plugin".into(),
            path: Some(path.display().to_string()),
        });
    }

    skills
}

/// Parse skill markdown: extract name/description from YAML frontmatter or headings.
fn parse_skill_markdown(default_name: &str, content: &str) -> (String, String) {
    let mut name = default_name.to_string();
    let mut description = String::new();

    // Try YAML frontmatter
    if content.starts_with("---") {
        if let Some(end) = content[3..].find("---") {
            let frontmatter = &content[3..3 + end];
            for line in frontmatter.lines() {
                let line = line.trim();
                if let Some(val) = line.strip_prefix("name:") {
                    name = val.trim().trim_matches('"').trim_matches('\'').to_string();
                } else if let Some(val) = line.strip_prefix("description:") {
                    description = val.trim().trim_matches('"').trim_matches('\'').to_string();
                }
            }
        }
    }

    // Fallback: use first heading as name, first paragraph as description
    if description.is_empty() {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') && name == default_name {
                name = line.trim_start_matches('#').trim().to_string();
            } else if !line.is_empty() && !line.starts_with('#') && !line.starts_with("---") {
                description = line.to_string();
                break;
            }
        }
    }

    (name, description)
}

/// Load hooks from a hooks.json file.
fn load_hooks_from_file(path: &Path) -> HashMap<String, Vec<HookDefinition>> {
    if !path.exists() {
        return HashMap::new();
    }

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return HashMap::new(),
    };

    match serde_json::from_str::<HashMap<String, Vec<HookDefinition>>>(&text) {
        Ok(hooks) => hooks,
        Err(_) => HashMap::new(),
    }
}

/// Load MCP configs from a mcp.json file.
fn load_mcp_from_file(path: &Path) -> HashMap<String, McpServerConfig> {
    if !path.exists() {
        return HashMap::new();
    }

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return HashMap::new(),
    };

    match serde_json::from_str::<McpJsonConfig>(&text) {
        Ok(config) => config.mcp_servers,
        Err(_) => HashMap::new(),
    }
}

/// Plugin load errors.
#[derive(Debug, thiserror::Error)]
pub enum PluginLoadError {
    #[error("no manifest found at: {0}")]
    NoManifest(String),
    #[error("IO error: {0}")]
    Io(String),
    #[error("parse error: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ──────────────────────────────────────────────────────

    /// Create a minimal plugin directory with plugin.json and return the path.
    fn create_plugin_dir(root: &std::path::Path, name: &str) -> std::path::PathBuf {
        let plugin_dir = root.join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            serde_json::json!({
                "name": name,
                "version": "1.0.0",
                "description": "A test plugin"
            })
            .to_string(),
        )
        .unwrap();
        plugin_dir
    }

    // ── parse_skill_markdown ────────────────────────────────────────

    #[test]
    fn test_parse_skill_markdown_yaml_frontmatter() {
        let content = "---\nname: hello\ndescription: greet the user\n---\nHello!";
        let (name, desc) = parse_skill_markdown("fallback", content);
        assert_eq!(name, "hello");
        assert_eq!(desc, "greet the user");
    }

    #[test]
    fn test_parse_skill_markdown_heading_fallback() {
        let content = "# My Skill\nThis does something cool.";
        let (name, desc) = parse_skill_markdown("default", content);
        assert_eq!(name, "My Skill");
        assert_eq!(desc, "This does something cool.");
    }

    #[test]
    fn test_parse_skill_markdown_default_name_when_no_heading() {
        let content = "Just some plain text.";
        let (name, desc) = parse_skill_markdown("fallback_name", content);
        assert_eq!(name, "fallback_name");
        assert_eq!(desc, "Just some plain text.");
    }

    #[test]
    fn test_parse_skill_markdown_frontmatter_with_quotes() {
        let content = "---\nname: \"quoted-name\"\ndescription: 'single-quoted'\n---\nbody";
        let (name, desc) = parse_skill_markdown("default", content);
        assert_eq!(name, "quoted-name");
        assert_eq!(desc, "single-quoted");
    }

    // ── load_hooks_from_file ────────────────────────────────────────

    #[test]
    fn test_load_hooks_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_path = dir.path().join("hooks.json");
        std::fs::write(
            &hooks_path,
            r#"{
                "pre_tool_use": [
                    {"type": "command", "command": "echo ok"}
                ]
            }"#,
        )
        .unwrap();

        let hooks = load_hooks_from_file(&hooks_path);
        assert!(hooks.contains_key("pre_tool_use"));
        assert_eq!(hooks["pre_tool_use"].len(), 1);
    }

    #[test]
    fn test_load_hooks_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = load_hooks_from_file(&dir.path().join("nonexistent.json"));
        assert!(hooks.is_empty());
    }

    #[test]
    fn test_load_hooks_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        std::fs::write(&path, "not json").unwrap();

        let hooks = load_hooks_from_file(&path);
        assert!(hooks.is_empty());
    }

    // ── load_mcp_from_file ──────────────────────────────────────────

    #[test]
    fn test_load_mcp_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let mcp_path = dir.path().join("mcp.json");
        std::fs::write(
            &mcp_path,
            r#"{
                "mcpServers": {
                    "my-server": {
                        "type": "stdio",
                        "command": "node",
                        "args": ["server.js"]
                    }
                }
            }"#,
        )
        .unwrap();

        let mcp = load_mcp_from_file(&mcp_path);
        assert!(mcp.contains_key("my-server"));
    }

    #[test]
    fn test_load_mcp_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let mcp = load_mcp_from_file(&dir.path().join("nonexistent.json"));
        assert!(mcp.is_empty());
    }

    #[test]
    fn test_load_mcp_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(&path, "{{bad").unwrap();

        let mcp = load_mcp_from_file(&path);
        assert!(mcp.is_empty());
    }

    // ── load_static_plugin ──────────────────────────────────────────

    #[test]
    fn test_load_static_plugin_with_skills() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = create_plugin_dir(dir.path(), "my-plugin");

        let skills_dir = plugin_dir.join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("hello.md"),
            "---\nname: hello\ndescription: greet\n---\nHello!",
        )
        .unwrap();

        let enabled: HashMap<String, bool> = HashMap::new();
        let plugin = load_static_plugin(&plugin_dir, &enabled).unwrap();

        assert_eq!(plugin.manifest.name, "my-plugin");
        assert_eq!(plugin.skills.len(), 1);
        assert_eq!(plugin.skills[0].name, "hello");
        assert_eq!(plugin.skills[0].description, "greet");
        assert!(plugin.enabled); // enabled_by_default is true
    }

    #[test]
    fn test_load_static_plugin_disabled_via_map() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = create_plugin_dir(dir.path(), "disabled-plugin");

        let mut enabled = HashMap::new();
        enabled.insert("disabled-plugin".to_string(), false);

        let plugin = load_static_plugin(&plugin_dir, &enabled).unwrap();
        assert!(!plugin.enabled);
    }

    #[test]
    fn test_load_static_plugin_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("bad-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.json"), "not valid json").unwrap();

        let enabled: HashMap<String, bool> = HashMap::new();
        let result = load_static_plugin(&plugin_dir, &enabled);
        assert!(result.is_err());
    }

    #[test]
    fn test_load_static_plugin_no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("no-manifest");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        let enabled: HashMap<String, bool> = HashMap::new();
        let result = load_static_plugin(&plugin_dir, &enabled);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PluginLoadError::NoManifest(_)));
    }

    #[test]
    fn test_load_static_plugin_empty_skills_dir() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = create_plugin_dir(dir.path(), "empty-skills");

        // skills dir doesn't exist — should still load with zero skills
        let enabled: HashMap<String, bool> = HashMap::new();
        let plugin = load_static_plugin(&plugin_dir, &enabled).unwrap();
        assert!(plugin.skills.is_empty());
        assert!(plugin.hooks.is_empty());
        assert!(plugin.mcp_servers.is_empty());
    }

    #[test]
    fn test_load_static_plugin_with_hooks_and_mcp() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = create_plugin_dir(dir.path(), "full-plugin");

        // Add hooks.json
        std::fs::write(
            plugin_dir.join("hooks.json"),
            r#"{"session_start": [{"type": "command", "command": "echo hi"}]}"#,
        )
        .unwrap();

        // Add mcp.json
        std::fs::write(
            plugin_dir.join("mcp.json"),
            r#"{"mcpServers": {"srv": {"type": "stdio", "command": "node", "args": []}}}"#,
        )
        .unwrap();

        let enabled: HashMap<String, bool> = HashMap::new();
        let plugin = load_static_plugin(&plugin_dir, &enabled).unwrap();
        assert!(!plugin.hooks.is_empty());
        assert!(plugin.hooks.contains_key("session_start"));
        assert!(plugin.mcp_servers.contains_key("srv"));
    }
}
