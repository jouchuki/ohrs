//! YAML-driven agent definitions loader for the coordinator.
//!
//! Loads `AgentDefinition` structs from `<project>/.claude/agents/<name>.yaml`
//! and `~/.claude/agents/<name>.yaml`.  Project-root definitions win on name
//! collision.  Individual file errors are logged as warnings and skipped.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum AgentDefError {
    #[error("I/O error while scanning agents directory: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Controls which tools an agent is allowed to invoke.
///
/// Maps to the Python `tools` + `disallowed_tools` pair, but expressed as a
/// mutually-exclusive tagged enum for clarity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolPolicy {
    /// All tools are permitted (default when the `tools` key is absent).
    AllowAll,
    /// Only the listed tools are permitted (Python: `tools: [...]`).
    AllowList {
        #[serde(default)]
        list: Vec<String>,
    },
    /// All tools except the listed ones are permitted (Python: `disallowed_tools: [...]`).
    DenyList {
        #[serde(default)]
        list: Vec<String>,
    },
}

impl Default for ToolPolicy {
    fn default() -> Self {
        ToolPolicy::AllowAll
    }
}

/// How the agent should be isolated from the parent process.
/// Maps to Python `isolation` field with values `"worktree"` | `"remote"` plus
/// a Rust-native `Subprocess` variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    /// No special isolation (default).
    #[default]
    #[serde(rename = "none")]
    None,
    /// Run the agent in a separate git worktree.
    Worktree,
    /// Run the agent in a separate subprocess.
    Subprocess,
}

/// Controls whether the agent's memory is shared with the parent session.
/// Maps to Python `memory` field with values `"user"` | `"project"` | `"local"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    /// Inherit the parent session's memory (default).
    #[default]
    Inherit,
    /// Use a fresh, isolated memory context.
    Isolated,
}

// ---------------------------------------------------------------------------
// AgentDefinition
// ---------------------------------------------------------------------------

/// Full configuration for a named agent, loaded from a YAML file.
///
/// Field mapping to the Python `AgentDefinition` / TypeScript `BaseAgentDefinition`:
/// - `name`          → `agentType` / file stem
/// - `description`   → `whenToUse`
/// - `system_prompt` → `getSystemPrompt()` return value
/// - `tools`         → `ToolPolicy` (wraps `tools` + `disallowed_tools`)
/// - `hooks`         → `hooks`
/// - `color`         → `color`
/// - `model`         → `model`
/// - `effort`        → `effort`
/// - `permission_mode` → `permissionMode`
/// - `max_turns`     → `maxTurns`
/// - `skills`        → `skills`
/// - `mcp_servers`   → `mcpServers`
/// - `required_mcp_servers` → `requiredMcpServers`
/// - `background`    → `background`
/// - `initial_prompt` → `initialPrompt`
/// - `memory_scope`  → (maps from Python `memory`)
/// - `isolation_mode` → (maps from Python `isolation`)
/// - `critical_system_reminder` → `criticalSystemReminder_EXPERIMENTAL`
/// - `omit_claude_md` → `omitClaudeMd`
/// - `subagent_type` → routing key
/// - `source`        → filesystem path
#[derive(Debug, Clone)]
pub struct AgentDefinition {
    /// File basename without the `.yaml` extension; used as the lookup key.
    pub name: String,
    /// Human-readable description (when to use this agent).
    pub description: String,
    /// Optional model override (e.g. `"claude-sonnet-4-6"`).
    pub model: Option<String>,
    /// Optional effort hint: `"low"`, `"medium"`, or `"high"`.
    pub effort: Option<String>,
    /// Optional system prompt injected at the start of each session.
    pub system_prompt: Option<String>,
    /// Tool-access policy (defaults to `AllowAll`).
    pub tools: ToolPolicy,
    /// Pre-rendered hook configuration (hook-event → arbitrary JSON).
    pub hooks: HashMap<String, serde_json::Value>,
    /// Process / sandbox isolation mode.
    pub isolation_mode: IsolationMode,
    /// Memory sharing scope.
    pub memory_scope: MemoryScope,
    /// Maximum agentic turns before the harness stops the agent.
    pub max_turns: Option<u32>,
    /// Wall-clock timeout in seconds.
    pub timeout_seconds: Option<u32>,
    /// The filesystem path the definition was loaded from.
    pub source: PathBuf,

    // --- parity with Python reference ---

    /// Permission mode override (e.g. `"default"`, `"acceptEdits"`, `"bypassPermissions"`).
    pub permission_mode: Option<String>,
    /// UI accent color (e.g. `"red"`, `"blue"`, `"green"`).
    pub color: Option<String>,
    /// Skill names the agent should load.
    pub skills: Vec<String>,
    /// MCP server references or inline configs (arbitrary JSON values).
    pub mcp_servers: Vec<serde_json::Value>,
    /// MCP server name patterns that must be present for this agent to be usable.
    pub required_mcp_servers: Vec<String>,
    /// Run this agent as a background task by default when spawned.
    pub background: bool,
    /// String prepended to the first user turn.
    pub initial_prompt: Option<String>,
    /// Short message re-injected at every user turn.
    pub critical_system_reminder: Option<String>,
    /// Skip CLAUDE.md injection for this agent.
    pub omit_claude_md: bool,
    /// Routing key used by the harness (defaults to `name`).
    pub subagent_type: String,
}

// ---------------------------------------------------------------------------
// Helper: Python-compatible tool list deserialization
//
// Python accepts `tools` as either a YAML list of strings  (tools: [a, b])
// or as a tagged enum object (tools: {type: allow_list, list: [a, b]}).
// We bridge these via a custom `RawTools` type.
// ---------------------------------------------------------------------------

/// `tools` in YAML can be:
/// - absent                             → AllowAll
/// - a list of strings `[a, b]`         → AllowList { list: [a, b] }  (Python style)
/// - a tagged object `{type: ..., list}` → ToolPolicy (native Rust style)
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawTools {
    /// Python-style `tools: [bash, grep]` or `tools: ['*']`
    List(Vec<String>),
    /// Native tagged-enum style `tools: {type: allow_list, list: [bash]}`
    Policy(ToolPolicy),
}

impl RawTools {
    fn into_policy(self) -> ToolPolicy {
        match self {
            RawTools::Policy(p) => p,
            RawTools::List(list) => {
                if list.is_empty() || list == ["*"] {
                    ToolPolicy::AllowAll
                } else {
                    ToolPolicy::AllowList { list }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Raw serde struct (what sits in the YAML file)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawAgentDef {
    #[serde(default)]
    description: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    /// Absent → AllowAll.
    /// Python style: `tools: [bash, grep]` → AllowList.
    /// Rust style: `tools: {type: allow_list, list: [...]}`.
    #[serde(default)]
    tools: Option<RawTools>,
    /// Python `disallowed_tools` / `disallowedTools` → converted to DenyList.
    /// Ignored when `tools` is already a DenyList/AllowList.
    #[serde(default, alias = "disallowedTools")]
    disallowed_tools: Option<Vec<String>>,
    #[serde(default)]
    hooks: HashMap<String, serde_json::Value>,
    /// `isolation_mode` (Rust) or `isolation` (Python).
    #[serde(default, alias = "isolation")]
    isolation_mode: IsolationMode,
    /// `memory_scope` (Rust) or `memory` (Python).
    #[serde(default, alias = "memory")]
    memory_scope: MemoryScope,
    /// `max_turns` (Rust/Python snake_case) or `maxTurns` (Python camelCase).
    #[serde(default, alias = "maxTurns")]
    max_turns: Option<u32>,
    #[serde(default)]
    timeout_seconds: Option<u32>,

    // --- parity fields ---
    #[serde(default, alias = "permissionMode")]
    permission_mode: Option<String>,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default, alias = "mcpServers")]
    mcp_servers: Vec<serde_json::Value>,
    #[serde(default, alias = "requiredMcpServers")]
    required_mcp_servers: Vec<String>,
    #[serde(default)]
    background: bool,
    #[serde(default, alias = "initialPrompt")]
    initial_prompt: Option<String>,
    #[serde(default, alias = "criticalSystemReminder")]
    critical_system_reminder: Option<String>,
    #[serde(default, alias = "omitClaudeMd")]
    omit_claude_md: bool,
    #[serde(default)]
    subagent_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// In-memory collection of loaded agent definitions, keyed by name.
pub struct AgentDefinitionRegistry {
    defs: HashMap<String, AgentDefinition>,
}

impl AgentDefinitionRegistry {
    fn new(defs: HashMap<String, AgentDefinition>) -> Self {
        Self { defs }
    }

    /// Look up a definition by name.
    pub fn get(&self, name: &str) -> Option<&AgentDefinition> {
        self.defs.get(name)
    }

    /// Return all definitions in unspecified order.
    pub fn list(&self) -> Vec<&AgentDefinition> {
        self.defs.values().collect()
    }

    /// Return all registered names in unspecified order.
    pub fn names(&self) -> Vec<&str> {
        self.defs.keys().map(String::as_str).collect()
    }
}

// ---------------------------------------------------------------------------
// Loader
// ---------------------------------------------------------------------------

/// Walks one or more `agents/` directories and parses every `.yaml` file.
pub struct AgentDefinitionLoader {
    /// Search roots, in priority order (lowest first, highest last).
    /// When two roots define the same name the last root's definition wins.
    roots: Vec<PathBuf>,
}

impl AgentDefinitionLoader {
    /// Create a loader with the standard two-root layout:
    /// 1. `~/.claude/agents/`  (home, lower priority)
    /// 2. `<project_root>/.claude/agents/`  (project, higher priority)
    pub fn new(project_root: &Path) -> Self {
        let mut roots: Vec<PathBuf> = Vec::new();

        // Home root (lowest priority)
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join(".claude").join("agents"));
        }

        // Project root (highest priority — overrides home)
        roots.push(project_root.join(".claude").join("agents"));

        Self { roots }
    }

    /// Create a loader with an explicit list of roots (lowest → highest priority).
    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }

    /// Walk all roots, parse every `.yaml` file found, and return a registry.
    ///
    /// Files that cannot be parsed are skipped after emitting a `tracing::warn!`.
    /// Higher-priority roots (later in the `roots` vector) override lower ones.
    pub async fn load_all(&self) -> Result<AgentDefinitionRegistry, AgentDefError> {
        let mut defs: HashMap<String, AgentDefinition> = HashMap::new();

        for root in &self.roots {
            if !root.exists() {
                continue;
            }

            let mut read_dir = match tokio::fs::read_dir(root).await {
                Ok(rd) => rd,
                Err(e) => {
                    warn!("agent_definitions: cannot read directory {}: {}", root.display(), e);
                    continue;
                }
            };

            // Collect entries so we can sort them for deterministic ordering.
            let mut entries: Vec<PathBuf> = Vec::new();
            loop {
                match read_dir.next_entry().await {
                    Ok(Some(entry)) => {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                            entries.push(path);
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        warn!("agent_definitions: error reading entry in {}: {}", root.display(), e);
                    }
                }
            }
            entries.sort();

            for path in entries {
                match Self::load_file(&path).await {
                    Ok(def) => {
                        defs.insert(def.name.clone(), def);
                    }
                    Err(e) => {
                        warn!(
                            "agent_definitions: skipping {} — {e}",
                            path.display()
                        );
                    }
                }
            }
        }

        Ok(AgentDefinitionRegistry::new(defs))
    }

    /// Parse a single YAML file into an `AgentDefinition`.
    async fn load_file(path: &Path) -> Result<AgentDefinition, Box<dyn std::error::Error + Send + Sync>> {
        let content = tokio::fs::read_to_string(path).await?;
        let raw: RawAgentDef = serde_yaml::from_str(&content)?;

        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("file has no stem")?
            .to_string();

        let subagent_type = raw.subagent_type.unwrap_or_else(|| name.clone());

        // Resolve tool policy: raw `tools` wins; fall back to `disallowed_tools`
        // if only that field is present (Python compatibility).
        let tools = match raw.tools {
            Some(raw_tools) => raw_tools.into_policy(),
            None => match raw.disallowed_tools {
                Some(list) if !list.is_empty() => ToolPolicy::DenyList { list },
                _ => ToolPolicy::AllowAll,
            },
        };

        Ok(AgentDefinition {
            name,
            description: raw.description,
            model: raw.model,
            effort: raw.effort,
            system_prompt: raw.system_prompt,
            tools,
            hooks: raw.hooks,
            isolation_mode: raw.isolation_mode,
            memory_scope: raw.memory_scope,
            max_turns: raw.max_turns,
            timeout_seconds: raw.timeout_seconds,
            source: path.to_path_buf(),
            permission_mode: raw.permission_mode,
            color: raw.color,
            skills: raw.skills,
            mcp_servers: raw.mcp_servers,
            required_mcp_servers: raw.required_mcp_servers,
            background: raw.background,
            initial_prompt: raw.initial_prompt,
            critical_system_reminder: raw.critical_system_reminder,
            omit_claude_md: raw.omit_claude_md,
            subagent_type,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_yaml(dir: &Path, name: &str, content: &str) {
        let path = dir.join(format!("{name}.yaml"));
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    // Helper: run the async loader synchronously in tests.
    fn run<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Runtime::new().unwrap().block_on(f)
    }

    // -----------------------------------------------------------------------
    // Test 1: two fully-populated definitions load correctly
    // -----------------------------------------------------------------------
    #[test]
    fn test_load_two_full_definitions() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        write_yaml(
            &agents_dir,
            "foo",
            r#"
description: PR reviewer
model: claude-sonnet-4-6
effort: high
system_prompt: |
  You review pull requests carefully...
tools:
  type: allow_list
  list: [bash, file_read, grep]
hooks:
  PreToolUse:
    - type: command
      command: "echo running tool"
isolation_mode: worktree
memory_scope: isolated
max_turns: 50
timeout_seconds: 600
permission_mode: acceptEdits
color: blue
skills: [git, review]
mcp_servers:
  - github
background: false
initial_prompt: "Start by reading the PR diff."
critical_system_reminder: "Be thorough."
omit_claude_md: true
subagent_type: pr-reviewer
"#,
        );

        write_yaml(
            &agents_dir,
            "bar",
            r#"
description: Code writer
model: claude-haiku
effort: low
system_prompt: Write clean code.
tools:
  type: deny_list
  list: [bash]
isolation_mode: subprocess
memory_scope: inherit
max_turns: 10
timeout_seconds: 120
background: true
"#,
        );

        let loader = AgentDefinitionLoader::with_roots(vec![agents_dir.clone()]);
        let registry = run(loader.load_all()).unwrap();

        let foo = registry.get("foo").expect("foo should be loaded");
        assert_eq!(foo.name, "foo");
        assert_eq!(foo.description, "PR reviewer");
        assert_eq!(foo.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(foo.effort.as_deref(), Some("high"));
        assert!(foo.system_prompt.as_deref().unwrap().contains("carefully"));
        assert_eq!(
            foo.tools,
            ToolPolicy::AllowList {
                list: vec!["bash".to_string(), "file_read".to_string(), "grep".to_string()]
            }
        );
        assert_eq!(foo.isolation_mode, IsolationMode::Worktree);
        assert_eq!(foo.memory_scope, MemoryScope::Isolated);
        assert_eq!(foo.max_turns, Some(50));
        assert_eq!(foo.timeout_seconds, Some(600));
        assert!(!foo.hooks.is_empty());
        assert_eq!(foo.permission_mode.as_deref(), Some("acceptEdits"));
        assert_eq!(foo.color.as_deref(), Some("blue"));
        assert_eq!(foo.skills, vec!["git".to_string(), "review".to_string()]);
        assert_eq!(foo.mcp_servers.len(), 1);
        assert!(!foo.background);
        assert_eq!(foo.initial_prompt.as_deref(), Some("Start by reading the PR diff."));
        assert_eq!(foo.critical_system_reminder.as_deref(), Some("Be thorough."));
        assert!(foo.omit_claude_md);
        assert_eq!(foo.subagent_type, "pr-reviewer");

        let bar = registry.get("bar").expect("bar should be loaded");
        assert_eq!(bar.name, "bar");
        assert_eq!(bar.isolation_mode, IsolationMode::Subprocess);
        assert_eq!(bar.memory_scope, MemoryScope::Inherit);
        assert_eq!(
            bar.tools,
            ToolPolicy::DenyList {
                list: vec!["bash".to_string()]
            }
        );
        assert!(bar.background);
        // subagent_type should default to name when absent
        assert_eq!(bar.subagent_type, "bar");

        assert_eq!(registry.list().len(), 2);
    }

    // -----------------------------------------------------------------------
    // Test 2: missing optional fields get sensible defaults
    // -----------------------------------------------------------------------
    #[test]
    fn test_missing_optional_fields_default_sensibly() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        write_yaml(
            &agents_dir,
            "minimal",
            r#"
description: Minimal agent
"#,
        );

        let loader = AgentDefinitionLoader::with_roots(vec![agents_dir.clone()]);
        let registry = run(loader.load_all()).unwrap();

        let def = registry.get("minimal").expect("minimal should load");
        assert_eq!(def.tools, ToolPolicy::AllowAll, "tools should default to AllowAll");
        assert_eq!(def.isolation_mode, IsolationMode::None, "isolation_mode should default to None");
        assert_eq!(def.memory_scope, MemoryScope::Inherit, "memory_scope should default to Inherit");
        assert!(def.model.is_none());
        assert!(def.effort.is_none());
        assert!(def.system_prompt.is_none());
        assert!(def.max_turns.is_none());
        assert!(def.timeout_seconds.is_none());
        assert!(def.hooks.is_empty());
        assert!(def.permission_mode.is_none());
        assert!(def.color.is_none());
        assert!(def.skills.is_empty());
        assert!(def.mcp_servers.is_empty());
        assert!(def.required_mcp_servers.is_empty());
        assert!(!def.background);
        assert!(def.initial_prompt.is_none());
        assert!(def.critical_system_reminder.is_none());
        assert!(!def.omit_claude_md);
        assert_eq!(def.subagent_type, "minimal", "subagent_type should default to name");
    }

    // -----------------------------------------------------------------------
    // Test 3: bad YAML in one file does not prevent others from loading
    // -----------------------------------------------------------------------
    #[test]
    fn test_bad_yaml_skipped_others_load() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        // Deliberately broken YAML
        write_yaml(
            &agents_dir,
            "broken",
            r#"
description: [unclosed bracket
: bad key
"#,
        );

        write_yaml(
            &agents_dir,
            "good",
            r#"
description: A good agent
"#,
        );

        let loader = AgentDefinitionLoader::with_roots(vec![agents_dir.clone()]);
        let registry = run(loader.load_all()).unwrap();

        assert!(registry.get("broken").is_none(), "broken YAML should be skipped");
        assert!(registry.get("good").is_some(), "good file should still load");
    }

    // -----------------------------------------------------------------------
    // Test 4: project root overrides home root on name collision
    // -----------------------------------------------------------------------
    #[test]
    fn test_project_overrides_home() {
        let tmp = TempDir::new().unwrap();

        let home_agents = tmp.path().join("home_agents");
        std::fs::create_dir_all(&home_agents).unwrap();
        write_yaml(
            &home_agents,
            "shared",
            r#"
description: Home version
model: home-model
"#,
        );

        let project_agents = tmp.path().join("project_agents");
        std::fs::create_dir_all(&project_agents).unwrap();
        write_yaml(
            &project_agents,
            "shared",
            r#"
description: Project version
model: project-model
"#,
        );

        // home_agents has lower priority (index 0), project_agents wins (index 1)
        let loader = AgentDefinitionLoader::with_roots(vec![
            home_agents.clone(),
            project_agents.clone(),
        ]);
        let registry = run(loader.load_all()).unwrap();

        let def = registry.get("shared").expect("shared should exist");
        assert_eq!(def.model.as_deref(), Some("project-model"), "project should win");
        assert_eq!(def.description, "Project version");
    }

    // -----------------------------------------------------------------------
    // Test 5: ToolPolicy round-trips through YAML for all 3 variants
    // -----------------------------------------------------------------------
    #[test]
    fn test_tool_policy_roundtrip() {
        // AllowAll
        let yaml_allow_all = "type: allow_all\n";
        let parsed: ToolPolicy = serde_yaml::from_str(yaml_allow_all).unwrap();
        assert_eq!(parsed, ToolPolicy::AllowAll);

        // AllowList
        let yaml_allow_list = "type: allow_list\nlist: [bash, grep]\n";
        let parsed: ToolPolicy = serde_yaml::from_str(yaml_allow_list).unwrap();
        assert_eq!(
            parsed,
            ToolPolicy::AllowList {
                list: vec!["bash".to_string(), "grep".to_string()]
            }
        );

        // DenyList
        let yaml_deny_list = "type: deny_list\nlist: [bash]\n";
        let parsed: ToolPolicy = serde_yaml::from_str(yaml_deny_list).unwrap();
        assert_eq!(
            parsed,
            ToolPolicy::DenyList {
                list: vec!["bash".to_string()]
            }
        );

        // Round-trip: serialize then deserialize
        let original = ToolPolicy::AllowList {
            list: vec!["tool_a".to_string(), "tool_b".to_string()],
        };
        let serialized = serde_yaml::to_string(&original).unwrap();
        let deserialized: ToolPolicy = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(original, deserialized, "round-trip should be lossless");
    }

    // -----------------------------------------------------------------------
    // Test 6: empty / nonexistent roots are silently skipped
    // -----------------------------------------------------------------------
    #[test]
    fn test_nonexistent_roots_are_skipped() {
        let loader = AgentDefinitionLoader::with_roots(vec![
            PathBuf::from("/nonexistent/path/that/does/not/exist"),
        ]);
        let registry = run(loader.load_all()).unwrap();
        assert_eq!(registry.list().len(), 0);
    }

    // -----------------------------------------------------------------------
    // Test 7: names() returns all registered names
    // -----------------------------------------------------------------------
    #[test]
    fn test_names_returns_all() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        for name in &["alpha", "beta", "gamma"] {
            write_yaml(&agents_dir, name, &format!("description: {name} agent\n"));
        }

        let loader = AgentDefinitionLoader::with_roots(vec![agents_dir.clone()]);
        let registry = run(loader.load_all()).unwrap();

        let mut names = registry.names();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
    }

    // -----------------------------------------------------------------------
    // Test 8: Python-style `tools: [list]` and `disallowed_tools` parsing
    // -----------------------------------------------------------------------
    #[test]
    fn test_python_style_tools_and_disallowed() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        // Python-style allow list
        write_yaml(
            &agents_dir,
            "python_allow",
            r#"
description: Python allow style
tools: [bash, grep, read]
"#,
        );

        // Python-style disallowed_tools → DenyList
        write_yaml(
            &agents_dir,
            "python_deny",
            r#"
description: Python deny style
disallowed_tools: [bash]
"#,
        );

        // `tools: ['*']` → AllowAll
        write_yaml(
            &agents_dir,
            "python_star",
            r#"
description: Python star all
tools: ['*']
"#,
        );

        // camelCase alias `disallowedTools`
        write_yaml(
            &agents_dir,
            "camel_deny",
            r#"
description: Camel deny
disallowedTools: [bash, rm]
"#,
        );

        let loader = AgentDefinitionLoader::with_roots(vec![agents_dir.clone()]);
        let registry = run(loader.load_all()).unwrap();

        let allow = registry.get("python_allow").unwrap();
        assert_eq!(
            allow.tools,
            ToolPolicy::AllowList {
                list: vec!["bash".to_string(), "grep".to_string(), "read".to_string()]
            }
        );

        let deny = registry.get("python_deny").unwrap();
        assert_eq!(
            deny.tools,
            ToolPolicy::DenyList {
                list: vec!["bash".to_string()]
            }
        );

        let star = registry.get("python_star").unwrap();
        assert_eq!(star.tools, ToolPolicy::AllowAll);

        let camel_deny = registry.get("camel_deny").unwrap();
        assert_eq!(
            camel_deny.tools,
            ToolPolicy::DenyList {
                list: vec!["bash".to_string(), "rm".to_string()]
            }
        );
    }

    // -----------------------------------------------------------------------
    // Test 9: Python aliases for isolation / memory / maxTurns
    // -----------------------------------------------------------------------
    #[test]
    fn test_python_aliases_isolation_memory_maxturns() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        write_yaml(
            &agents_dir,
            "python_aliases",
            r#"
description: Python alias test
isolation: worktree
memory: isolated
maxTurns: 42
"#,
        );

        let loader = AgentDefinitionLoader::with_roots(vec![agents_dir.clone()]);
        let registry = run(loader.load_all()).unwrap();

        let def = registry.get("python_aliases").unwrap();
        assert_eq!(def.isolation_mode, IsolationMode::Worktree);
        assert_eq!(def.memory_scope, MemoryScope::Isolated);
        assert_eq!(def.max_turns, Some(42));
    }

    // -----------------------------------------------------------------------
    // Test 10: camelCase aliases work (mcpServers, permissionMode, etc.)
    // -----------------------------------------------------------------------
    #[test]
    fn test_camel_case_aliases() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        write_yaml(
            &agents_dir,
            "camel",
            r#"
description: Camel case test
permissionMode: bypassPermissions
mcpServers:
  - my-server
requiredMcpServers:
  - required-server
initialPrompt: "Hello camel"
criticalSystemReminder: "Important"
omitClaudeMd: true
"#,
        );

        let loader = AgentDefinitionLoader::with_roots(vec![agents_dir.clone()]);
        let registry = run(loader.load_all()).unwrap();

        let def = registry.get("camel").expect("camel should load");
        assert_eq!(def.permission_mode.as_deref(), Some("bypassPermissions"));
        assert_eq!(def.mcp_servers.len(), 1);
        assert_eq!(def.required_mcp_servers, vec!["required-server".to_string()]);
        assert_eq!(def.initial_prompt.as_deref(), Some("Hello camel"));
        assert_eq!(def.critical_system_reminder.as_deref(), Some("Important"));
        assert!(def.omit_claude_md);
    }
}
