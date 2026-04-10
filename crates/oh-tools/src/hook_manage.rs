//! Tool for the agent to manage its own hooks at runtime.
//!
//! Actions:
//! - `list`: Show all registered hooks
//! - `add`: Register a new hook on an event
//! - `clear`: Remove all hooks for an event (or all events)

use async_trait::async_trait;
use oh_types::hooks::{HookDefinition, HookEvent};
use oh_types::tools::{ToolExecutionContext, ToolResult};
use tracing::instrument;

pub struct HookManageTool;

#[async_trait]
impl crate::traits::Tool for HookManageTool {
    fn name(&self) -> &str {
        "HookManage"
    }

    fn description(&self) -> &str {
        "List, add, or remove lifecycle hooks at runtime. \
         Use action 'list' to see current hooks, 'add' to register a new hook, \
         or 'clear' to remove hooks for an event."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "add", "clear"],
                    "description": "Action to perform"
                },
                "event": {
                    "type": "string",
                    "description": "Hook event name (e.g. 'pre_tool_use', 'post_tool_use', 'session_start'). Required for 'add' and 'clear'."
                },
                "hook_type": {
                    "type": "string",
                    "enum": ["command", "http"],
                    "description": "Type of hook to add. Required for 'add'."
                },
                "command": {
                    "type": "string",
                    "description": "Shell command to run (for hook_type='command')"
                },
                "url": {
                    "type": "string",
                    "description": "URL to POST to (for hook_type='http')"
                },
                "matcher": {
                    "type": "string",
                    "description": "Optional glob pattern to filter which events trigger this hook (e.g. 'Bash' to only match Bash tool calls)"
                },
                "block_on_failure": {
                    "type": "boolean",
                    "description": "If true, a failed hook blocks the operation. Default false."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "description": "Timeout in seconds. Default 30."
                }
            },
            "required": ["action"]
        })
    }

    fn is_read_only(&self, arguments: &serde_json::Value) -> bool {
        arguments
            .get("action")
            .and_then(|v| v.as_str())
            .map(|a| a == "list")
            .unwrap_or(true)
    }

    #[instrument(skip(self, context), fields(tool = "HookManage"))]
    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let action = match arguments.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolResult::error("Missing required parameter: action"),
        };

        // Get the hook registry from context metadata
        let registry_handle = context
            .metadata
            .get("hook_registry")
            .and_then(|v| {
                // The registry handle is stored as a serialized pointer (set by cli.rs)
                // We use a different approach: look for it in a global or passed via Arc
                None::<()>
            });

        // For now, use the global approach — the hook registry is accessible
        // via the metadata as a JSON-serialized snapshot for 'list',
        // and via direct Arc<RwLock<HookRegistry>> for mutations.
        // We'll parse the hook definition from JSON and return instructions.

        match action {
            "list" => {
                // Return the current hooks as readable text
                // The actual registry is in the executor, we can read it from metadata
                if let Some(summary) = context.metadata.get("hook_summary").and_then(|v| v.as_str()) {
                    ToolResult::success(summary)
                } else {
                    ToolResult::success("No hooks currently registered. Use action='add' to register hooks.")
                }
            }
            "add" => {
                let event_str = match arguments.get("event").and_then(|v| v.as_str()) {
                    Some(e) => e,
                    None => return ToolResult::error("Missing required parameter: event"),
                };
                let hook_type = match arguments.get("hook_type").and_then(|v| v.as_str()) {
                    Some(t) => t,
                    None => return ToolResult::error("Missing required parameter: hook_type"),
                };

                // Validate event name
                let event: HookEvent = match serde_json::from_value(
                    serde_json::Value::String(event_str.to_string()),
                ) {
                    Ok(e) => e,
                    Err(_) => {
                        return ToolResult::error(format!(
                            "Unknown event: '{event_str}'. Valid events: session_start, session_end, \
                             pre_tool_use, post_tool_use, pre_api_request, post_api_response, \
                             query_turn_start, query_turn_end, error_occurred, etc."
                        ))
                    }
                };

                let matcher = arguments.get("matcher").and_then(|v| v.as_str()).map(String::from);
                let matcher_display = matcher.clone();
                let block = arguments
                    .get("block_on_failure")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let timeout = arguments
                    .get("timeout_seconds")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(30) as u32;

                let hook_def = match hook_type {
                    "command" => {
                        let cmd = match arguments.get("command").and_then(|v| v.as_str()) {
                            Some(c) => c,
                            None => return ToolResult::error("Missing required parameter: command (for hook_type='command')"),
                        };
                        HookDefinition::Command(oh_types::hooks::CommandHookDefinition {
                            r#type: "command".into(),
                            command: cmd.to_string(),
                            timeout_seconds: timeout,
                            matcher,
                            block_on_failure: block,
                        })
                    }
                    "http" => {
                        let url = match arguments.get("url").and_then(|v| v.as_str()) {
                            Some(u) => u,
                            None => return ToolResult::error("Missing required parameter: url (for hook_type='http')"),
                        };
                        HookDefinition::Http(oh_types::hooks::HttpHookDefinition {
                            r#type: "http".into(),
                            url: url.to_string(),
                            headers: std::collections::HashMap::new(),
                            timeout_seconds: timeout,
                            matcher,
                            block_on_failure: block,
                        })
                    }
                    other => {
                        return ToolResult::error(format!(
                            "Unsupported hook_type: '{other}'. Use 'command' or 'http'."
                        ))
                    }
                };

                // Serialize the hook definition as JSON so the caller (cli.rs) can
                // apply it to the actual registry
                let hook_json = serde_json::to_string_pretty(&hook_def).unwrap_or_default();
                let mut result_meta = std::collections::HashMap::new();
                result_meta.insert(
                    "hook_action".to_string(),
                    serde_json::json!({
                        "action": "add",
                        "event": event_str,
                        "hook": hook_def,
                    }),
                );

                ToolResult {
                    output: format!(
                        "Registered {} hook on '{}' event{}:\n{}",
                        hook_type,
                        event_str,
                        matcher_display.as_ref().map(|m| format!(" (matcher: {m})")).unwrap_or_default(),
                        hook_json,
                    ),
                    is_error: false,
                    metadata: result_meta,
                }
            }
            "clear" => {
                let event_str = arguments.get("event").and_then(|v| v.as_str());

                match event_str {
                    Some(e) => {
                        // Validate event
                        if serde_json::from_value::<HookEvent>(
                            serde_json::Value::String(e.to_string()),
                        ).is_err() {
                            return ToolResult::error(format!("Unknown event: '{e}'"));
                        }

                        let mut result_meta = std::collections::HashMap::new();
                        result_meta.insert(
                            "hook_action".to_string(),
                            serde_json::json!({
                                "action": "clear_event",
                                "event": e,
                            }),
                        );
                        ToolResult {
                            output: format!("Cleared all hooks for event '{e}'."),
                            is_error: false,
                            metadata: result_meta,
                        }
                    }
                    None => {
                        let mut result_meta = std::collections::HashMap::new();
                        result_meta.insert(
                            "hook_action".to_string(),
                            serde_json::json!({"action": "clear_all"}),
                        );
                        ToolResult {
                            output: "Cleared all hooks for all events.".to_string(),
                            is_error: false,
                            metadata: result_meta,
                        }
                    }
                }
            }
            other => ToolResult::error(format!(
                "Unknown action: '{other}'. Use 'list', 'add', or 'clear'."
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;
    use std::path::PathBuf;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext::new(PathBuf::from("/tmp"))
    }

    #[tokio::test]
    async fn test_list_no_hooks() {
        let tool = HookManageTool;
        let result = tool
            .execute(serde_json::json!({"action": "list"}), &ctx())
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("No hooks"));
    }

    #[tokio::test]
    async fn test_add_command_hook() {
        let tool = HookManageTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "action": "add",
                    "event": "pre_tool_use",
                    "hook_type": "command",
                    "command": "echo validating",
                    "matcher": "Bash"
                }),
                &ctx(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("Registered command hook"));
        assert!(result.output.contains("pre_tool_use"));
        assert!(result.metadata.contains_key("hook_action"));
    }

    #[tokio::test]
    async fn test_add_http_hook() {
        let tool = HookManageTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "action": "add",
                    "event": "post_tool_use",
                    "hook_type": "http",
                    "url": "https://example.com/webhook"
                }),
                &ctx(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("Registered http hook"));
    }

    #[tokio::test]
    async fn test_add_missing_command() {
        let tool = HookManageTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "action": "add",
                    "event": "pre_tool_use",
                    "hook_type": "command"
                }),
                &ctx(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("command"));
    }

    #[tokio::test]
    async fn test_add_invalid_event() {
        let tool = HookManageTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "action": "add",
                    "event": "not_a_real_event",
                    "hook_type": "command",
                    "command": "echo"
                }),
                &ctx(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("Unknown event"));
    }

    #[tokio::test]
    async fn test_clear_event() {
        let tool = HookManageTool;
        let result = tool
            .execute(
                serde_json::json!({"action": "clear", "event": "pre_tool_use"}),
                &ctx(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("Cleared"));
        assert!(result.metadata.contains_key("hook_action"));
    }

    #[tokio::test]
    async fn test_clear_all() {
        let tool = HookManageTool;
        let result = tool
            .execute(serde_json::json!({"action": "clear"}), &ctx())
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("all events"));
    }

    #[tokio::test]
    async fn test_is_read_only_list() {
        let tool = HookManageTool;
        assert!(tool.is_read_only(&serde_json::json!({"action": "list"})));
        assert!(!tool.is_read_only(&serde_json::json!({"action": "add"})));
        assert!(!tool.is_read_only(&serde_json::json!({"action": "clear"})));
    }

    #[tokio::test]
    async fn test_missing_action() {
        let tool = HookManageTool;
        let result = tool.execute(serde_json::json!({}), &ctx()).await;
        assert!(result.is_error);
    }
}
