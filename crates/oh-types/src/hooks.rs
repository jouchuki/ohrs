//! Hook event names, definition schemas, and result types.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Events that can trigger hooks — expanded to cover the full lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    SessionStart,
    SessionEnd,
    PreToolUse,
    PostToolUse,
    PreApiRequest,
    PostApiResponse,
    PrePermissionCheck,
    PostPermissionCheck,
    PluginLoaded,
    PluginUnloaded,
    McpConnected,
    McpDisconnected,
    TaskCreated,
    TaskCompleted,
    ContextCompacted,
    MemoryUpdated,
    CommandExecuted,
    SkillInvoked,
    QueryTurnStart,
    QueryTurnEnd,
    ErrorOccurred,
    PreUserMessage,
    PostUserMessage,
    PrePushMessage,
    PostPushMessage,
    PreSystemPrompt,
    PostSystemPrompt,
    ApiRetry,
    #[serde(rename = "api_error")]
    ApiError_,
    PermissionDenied,
    PermissionConfirmation,
    ToolInputValidation,
    ToolTimeout,
    #[serde(rename = "tool_error")]
    ToolError_,
    StreamStart,
    StreamEnd,
    SessionSave,
    SessionResume,
    PreCommand,
    PostCommand,
    PreClearHistory,
    PostClearHistory,
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| format!("{:?}", self));
        write!(f, "{}", s)
    }
}

/// A hook that executes a shell command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandHookDefinition {
    #[serde(default = "command_type_tag", skip_serializing)]
    pub r#type: String,
    pub command: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u32,
    pub matcher: Option<String>,
    #[serde(default)]
    pub block_on_failure: bool,
}

fn command_type_tag() -> String {
    "command".into()
}

/// A hook that asks the model to validate a condition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptHookDefinition {
    #[serde(default = "prompt_type_tag", skip_serializing)]
    pub r#type: String,
    pub prompt: String,
    pub model: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u32,
    pub matcher: Option<String>,
    #[serde(default = "default_block_true")]
    pub block_on_failure: bool,
}

fn prompt_type_tag() -> String {
    "prompt".into()
}

fn default_block_true() -> bool {
    true
}

/// A hook that POSTs the event payload to an HTTP endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpHookDefinition {
    #[serde(default = "http_type_tag", skip_serializing)]
    pub r#type: String,
    pub url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u32,
    pub matcher: Option<String>,
    #[serde(default)]
    pub block_on_failure: bool,
}

fn http_type_tag() -> String {
    "http".into()
}

/// A hook that performs a deeper model-based validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHookDefinition {
    #[serde(default = "agent_type_tag", skip_serializing)]
    pub r#type: String,
    pub prompt: String,
    pub model: Option<String>,
    #[serde(default = "default_agent_timeout")]
    pub timeout_seconds: u32,
    pub matcher: Option<String>,
    #[serde(default = "default_block_true")]
    pub block_on_failure: bool,
}

fn agent_type_tag() -> String {
    "agent".into()
}

fn default_timeout() -> u32 {
    30
}

fn default_agent_timeout() -> u32 {
    60
}

/// Union of hook definition types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HookDefinition {
    #[serde(rename = "command")]
    Command(CommandHookDefinition),
    #[serde(rename = "prompt")]
    Prompt(PromptHookDefinition),
    #[serde(rename = "http")]
    Http(HttpHookDefinition),
    #[serde(rename = "agent")]
    Agent(AgentHookDefinition),
}

impl HookDefinition {
    /// Return the matcher pattern, if any.
    pub fn matcher(&self) -> Option<&str> {
        match self {
            Self::Command(h) => h.matcher.as_deref(),
            Self::Prompt(h) => h.matcher.as_deref(),
            Self::Http(h) => h.matcher.as_deref(),
            Self::Agent(h) => h.matcher.as_deref(),
        }
    }

    /// Return the timeout in seconds.
    pub fn timeout_seconds(&self) -> u32 {
        match self {
            Self::Command(h) => h.timeout_seconds,
            Self::Prompt(h) => h.timeout_seconds,
            Self::Http(h) => h.timeout_seconds,
            Self::Agent(h) => h.timeout_seconds,
        }
    }

    /// Whether this hook blocks on failure.
    pub fn block_on_failure(&self) -> bool {
        match self {
            Self::Command(h) => h.block_on_failure,
            Self::Prompt(h) => h.block_on_failure,
            Self::Http(h) => h.block_on_failure,
            Self::Agent(h) => h.block_on_failure,
        }
    }

    /// Return the hook type tag string.
    pub fn hook_type(&self) -> &str {
        match self {
            Self::Command(_) => "command",
            Self::Prompt(_) => "prompt",
            Self::Http(_) => "http",
            Self::Agent(_) => "agent",
        }
    }
}

/// Result from a single hook execution.
#[derive(Debug, Clone)]
pub struct HookResult {
    pub hook_type: String,
    pub success: bool,
    pub output: String,
    pub blocked: bool,
    pub reason: String,
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Default for HookResult {
    fn default() -> Self {
        Self {
            hook_type: String::new(),
            success: true,
            output: String::new(),
            blocked: false,
            reason: String::new(),
            metadata: HashMap::new(),
        }
    }
}

/// Aggregated result for a hook event.
#[derive(Debug, Clone, Default)]
pub struct AggregatedHookResult {
    pub results: Vec<HookResult>,
}

impl AggregatedHookResult {
    /// Whether any hook blocked continuation.
    pub fn blocked(&self) -> bool {
        self.results.iter().any(|r| r.blocked)
    }

    /// The first blocking reason, if any.
    pub fn reason(&self) -> &str {
        for result in &self.results {
            if result.blocked {
                let reason = if result.reason.is_empty() {
                    &result.output
                } else {
                    &result.reason
                };
                if !reason.is_empty() {
                    return reason;
                }
            }
        }
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_event_serde_roundtrip() {
        let event = HookEvent::PreToolUse;
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(json, "\"pre_tool_use\"");
        let deser: HookEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deser, event);
    }

    #[test]
    fn test_hook_event_serde_all_variants() {
        let variants = vec![
            (HookEvent::SessionStart, "\"session_start\""),
            (HookEvent::SessionEnd, "\"session_end\""),
            (HookEvent::PostToolUse, "\"post_tool_use\""),
            (HookEvent::PreApiRequest, "\"pre_api_request\""),
            (HookEvent::ErrorOccurred, "\"error_occurred\""),
        ];
        for (event, expected) in variants {
            let json = serde_json::to_string(&event).unwrap();
            assert_eq!(json, expected);
        }
    }

    #[test]
    fn test_hook_event_display() {
        assert_eq!(format!("{}", HookEvent::SessionStart), "session_start");
        assert_eq!(format!("{}", HookEvent::PreToolUse), "pre_tool_use");
        assert_eq!(format!("{}", HookEvent::ErrorOccurred), "error_occurred");
    }

    #[test]
    fn test_hook_definition_command_accessors() {
        let def = HookDefinition::Command(CommandHookDefinition {
            r#type: "command".into(),
            command: "echo hi".into(),
            timeout_seconds: 10,
            matcher: Some("bash".into()),
            block_on_failure: true,
        });
        assert_eq!(def.matcher(), Some("bash"));
        assert_eq!(def.timeout_seconds(), 10);
        assert!(def.block_on_failure());
        assert_eq!(def.hook_type(), "command");
    }

    #[test]
    fn test_hook_definition_prompt_accessors() {
        let def = HookDefinition::Prompt(PromptHookDefinition {
            r#type: "prompt".into(),
            prompt: "check safety".into(),
            model: None,
            timeout_seconds: 30,
            matcher: None,
            block_on_failure: true,
        });
        assert_eq!(def.matcher(), None);
        assert_eq!(def.timeout_seconds(), 30);
        assert!(def.block_on_failure());
        assert_eq!(def.hook_type(), "prompt");
    }

    #[test]
    fn test_hook_definition_http_accessors() {
        let def = HookDefinition::Http(HttpHookDefinition {
            r#type: "http".into(),
            url: "https://example.com".into(),
            headers: HashMap::new(),
            timeout_seconds: 15,
            matcher: Some("write_file".into()),
            block_on_failure: false,
        });
        assert_eq!(def.matcher(), Some("write_file"));
        assert_eq!(def.timeout_seconds(), 15);
        assert!(!def.block_on_failure());
        assert_eq!(def.hook_type(), "http");
    }

    #[test]
    fn test_hook_definition_agent_accessors() {
        let def = HookDefinition::Agent(AgentHookDefinition {
            r#type: "agent".into(),
            prompt: "validate".into(),
            model: Some("claude-3".into()),
            timeout_seconds: 60,
            matcher: None,
            block_on_failure: true,
        });
        assert_eq!(def.hook_type(), "agent");
        assert_eq!(def.timeout_seconds(), 60);
    }

    #[test]
    fn test_hook_definition_command_serde_roundtrip() {
        let def = HookDefinition::Command(CommandHookDefinition {
            r#type: "command".into(),
            command: "ls".into(),
            timeout_seconds: 30,
            matcher: None,
            block_on_failure: false,
        });
        let json = serde_json::to_string(&def).unwrap();
        assert!(json.contains("\"type\":\"command\""));
        let deser: HookDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.hook_type(), "command");
        assert_eq!(deser.timeout_seconds(), 30);
    }

    #[test]
    fn test_hook_definition_deserialize_defaults() {
        let json = r#"{"type":"command","command":"echo hi"}"#;
        let def: HookDefinition = serde_json::from_str(json).unwrap();
        assert_eq!(def.timeout_seconds(), 30); // default_timeout
        assert!(!def.block_on_failure()); // default false for command
    }

    #[test]
    fn test_hook_definition_prompt_deserialize_defaults() {
        let json = r#"{"type":"prompt","prompt":"check"}"#;
        let def: HookDefinition = serde_json::from_str(json).unwrap();
        assert_eq!(def.timeout_seconds(), 30);
        assert!(def.block_on_failure()); // default_block_true for prompt
    }

    #[test]
    fn test_hook_result_default() {
        let result = HookResult::default();
        assert!(result.success);
        assert!(!result.blocked);
        assert!(result.output.is_empty());
        assert!(result.reason.is_empty());
        assert!(result.hook_type.is_empty());
        assert!(result.metadata.is_empty());
    }

    #[test]
    fn test_aggregated_hook_result_not_blocked() {
        let agg = AggregatedHookResult {
            results: vec![HookResult::default()],
        };
        assert!(!agg.blocked());
        assert_eq!(agg.reason(), "");
    }

    #[test]
    fn test_aggregated_hook_result_blocked_with_reason() {
        let agg = AggregatedHookResult {
            results: vec![HookResult {
                blocked: true,
                reason: "not allowed".into(),
                ..Default::default()
            }],
        };
        assert!(agg.blocked());
        assert_eq!(agg.reason(), "not allowed");
    }

    #[test]
    fn test_aggregated_hook_result_blocked_fallback_to_output() {
        let agg = AggregatedHookResult {
            results: vec![HookResult {
                blocked: true,
                reason: "".into(),
                output: "from output".into(),
                ..Default::default()
            }],
        };
        assert!(agg.blocked());
        assert_eq!(agg.reason(), "from output");
    }

    #[test]
    fn test_aggregated_hook_result_empty() {
        let agg = AggregatedHookResult::default();
        assert!(!agg.blocked());
        assert_eq!(agg.reason(), "");
    }

    #[test]
    fn test_aggregated_hook_result_multiple_blocked_returns_first() {
        let agg = AggregatedHookResult {
            results: vec![
                HookResult {
                    blocked: true,
                    reason: "first".into(),
                    ..Default::default()
                },
                HookResult {
                    blocked: true,
                    reason: "second".into(),
                    ..Default::default()
                },
            ],
        };
        assert_eq!(agg.reason(), "first");
    }
}
