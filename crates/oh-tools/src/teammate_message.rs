//! Inter-agent messaging tool.
//!
//! Writes a [`Message`](oh_swarm::Message) into a recipient teammate's
//! file-backed [`Mailbox`](oh_swarm::Mailbox) (the same mailbox an in-process
//! subagent's `IdleNotification` lands in) and fires the
//! [`HookEvent::SubagentMessage`](oh_types::hooks::HookEvent::SubagentMessage)
//! hook so blocks / webhooks / recording observe the exchange.
//!
//! Because tools don't hold a hook-executor handle, the hook is raised through
//! the existing `hook_action` metadata channel the engine already drains in
//! `apply_hook_action` (a `"fire_event"` action).

use async_trait::async_trait;
use oh_swarm::{Mailbox, Message, MessageKind, TeammateId};
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct TeammateMessageTool;

/// Map the optional `kind` argument to a [`MessageKind`]. Unknown / absent
/// values default to a free-form `AgentReply`.
fn message_kind(arg: Option<&str>) -> MessageKind {
    match arg {
        Some("user_turn") => MessageKind::UserTurn,
        Some("status") => MessageKind::Status,
        Some("stop") => MessageKind::Stop,
        Some("idle_notification") => MessageKind::IdleNotification,
        Some("agent_reply") | None => MessageKind::AgentReply,
        Some(other) => MessageKind::Custom(other.to_string()),
    }
}

#[async_trait]
impl crate::traits::Tool for TeammateMessageTool {
    fn name(&self) -> &str {
        "TeammateMessage"
    }

    fn description(&self) -> &str {
        "Send a message to another teammate agent's mailbox (fires the subagent_message hook)."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "to": {"type": "string", "description": "Recipient teammate/agent id"},
                "content": {"type": "string", "description": "Message text"},
                "from": {"type": "string", "description": "Sender teammate/agent id (defaults to 'main')"},
                "kind": {
                    "type": "string",
                    "description": "Message kind",
                    "enum": ["agent_reply", "user_turn", "status", "stop", "idle_notification"]
                }
            },
            "required": ["to", "content"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let to = match arguments.get("to").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return ToolResult::error("Missing required parameter: to"),
        };
        let content = match arguments.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolResult::error("Missing required parameter: content"),
        };
        let from = arguments
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("main");
        let kind = message_kind(arguments.get("kind").and_then(|v| v.as_str()));

        // The recipient's mailbox lives under <tasks>/teammates, matching the
        // root the subagent manager and InProcessBackend use.
        let team_root = oh_config::get_tasks_dir().join("teammates");
        let recipient = TeammateId::new(to);
        let mailbox = Mailbox::for_agent(&team_root, &recipient);

        let msg = Message::new(
            TeammateId::new(from),
            recipient,
            kind.clone(),
            serde_json::json!({ "content": content }),
        );

        if let Err(e) = mailbox.send(&msg).await {
            return ToolResult::error(format!("Failed to deliver message: {e}"));
        }

        // Fire the SubagentMessage hook via the engine's hook_action channel.
        let mut result = ToolResult::success(format!("Message sent to {to}"));
        result.metadata.insert(
            "hook_action".to_string(),
            serde_json::json!({
                "action": "fire_event",
                "event": "subagent_message",
                "payload": {
                    "from": from,
                    "to": to,
                    "content": content,
                }
            }),
        );
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use std::path::PathBuf;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext::new(PathBuf::from("/tmp"))
    }

    #[test]
    fn test_name_and_not_read_only() {
        let tool = TeammateMessageTool;
        assert_eq!(tool.name(), "TeammateMessage");
        assert!(!tool.is_read_only(&serde_json::json!({})));
    }

    #[test]
    fn test_schema_required_fields() {
        let tool = TeammateMessageTool;
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "to"));
        assert!(required.iter().any(|v| v == "content"));
    }

    #[test]
    fn test_message_kind_mapping() {
        assert_eq!(message_kind(None), MessageKind::AgentReply);
        assert_eq!(message_kind(Some("status")), MessageKind::Status);
        assert_eq!(message_kind(Some("stop")), MessageKind::Stop);
        assert_eq!(
            message_kind(Some("idle_notification")),
            MessageKind::IdleNotification
        );
        assert_eq!(
            message_kind(Some("weird")),
            MessageKind::Custom("weird".to_string())
        );
    }

    #[tokio::test]
    async fn test_missing_params() {
        let tool = TeammateMessageTool;
        let r1 = tool
            .execute(serde_json::json!({"content": "hi"}), &ctx())
            .await;
        assert!(r1.is_error && r1.output.contains("to"));

        let r2 = tool
            .execute(serde_json::json!({"to": "peer"}), &ctx())
            .await;
        assert!(r2.is_error && r2.output.contains("content"));
    }

    #[tokio::test]
    async fn test_delivers_to_mailbox_and_requests_hook() {
        let _env_guard = crate::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Override the tasks dir so the mailbox writes into a temp location.
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("OPENHARNESSRS_DATA_DIR", dir.path());

        let tool = TeammateMessageTool;
        let to = format!("peer-{}", uuid::Uuid::new_v4());
        let result = tool
            .execute(
                serde_json::json!({"to": to, "content": "ping", "from": "leader"}),
                &ctx(),
            )
            .await;
        assert!(!result.is_error, "got: {}", result.output);
        assert_eq!(result.output, format!("Message sent to {to}"));

        // The hook_action requests SubagentMessage.
        let action = result.metadata.get("hook_action").unwrap();
        assert_eq!(action["action"], "fire_event");
        assert_eq!(action["event"], "subagent_message");
        assert_eq!(action["payload"]["content"], "ping");

        // The message landed in the recipient's mailbox.
        let team_root = oh_config::get_tasks_dir().join("teammates");
        let mailbox = Mailbox::for_agent(&team_root, &TeammateId::new(&to));
        let msgs = mailbox.peek_all().await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].kind, MessageKind::AgentReply);
        assert_eq!(msgs[0].from, TeammateId::new("leader"));
        assert_eq!(msgs[0].body["content"], "ping");

        std::env::remove_var("OPENHARNESSRS_DATA_DIR");
    }
}
