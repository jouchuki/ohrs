//! Events yielded by the query engine.

use std::collections::HashMap;

use serde::Serialize;

use crate::api::UsageSnapshot;
use crate::messages::ConversationMessage;

/// Incremental assistant text.
#[derive(Debug, Clone, Serialize)]
pub struct AssistantTextDelta {
    pub text: String,
}

/// Completed assistant turn.
#[derive(Debug, Clone, Serialize)]
pub struct AssistantTurnComplete {
    pub message: ConversationMessage,
    pub usage: UsageSnapshot,
}

/// The engine is about to execute a tool.
#[derive(Debug, Clone, Serialize)]
pub struct ToolExecutionStarted {
    pub tool_name: String,
    pub tool_input: HashMap<String, serde_json::Value>,
}

/// A tool has finished executing.
#[derive(Debug, Clone, Serialize)]
pub struct ToolExecutionCompleted {
    pub tool_name: String,
    pub output: String,
    pub is_error: bool,
}

/// Union of stream events yielded by the query engine.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    AssistantTextDelta(AssistantTextDelta),
    AssistantTurnComplete(AssistantTurnComplete),
    ToolExecutionStarted(ToolExecutionStarted),
    ToolExecutionCompleted(ToolExecutionCompleted),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{ConversationMessage, Role, ContentBlock, TextBlock};

    #[test]
    fn test_assistant_text_delta_clone() {
        let delta = AssistantTextDelta { text: "hello".into() };
        let cloned = delta.clone();
        assert_eq!(cloned.text, "hello");
    }

    #[test]
    fn test_assistant_turn_complete() {
        let turn = AssistantTurnComplete {
            message: ConversationMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text(TextBlock::new("done"))],
            },
            usage: UsageSnapshot { input_tokens: 10, output_tokens: 20, ..Default::default() },
        };
        assert_eq!(turn.usage.total_tokens(), 30);
        assert_eq!(turn.message.text(), "done");
    }

    #[test]
    fn test_tool_execution_started() {
        let event = ToolExecutionStarted {
            tool_name: "bash".into(),
            tool_input: HashMap::new(),
        };
        assert_eq!(event.tool_name, "bash");
    }

    #[test]
    fn test_tool_execution_completed() {
        let event = ToolExecutionCompleted {
            tool_name: "read".into(),
            output: "file contents".into(),
            is_error: false,
        };
        assert_eq!(event.tool_name, "read");
        assert!(!event.is_error);
    }

    #[test]
    fn test_stream_event_variants() {
        let delta = StreamEvent::AssistantTextDelta(AssistantTextDelta { text: "hi".into() });
        assert!(matches!(delta, StreamEvent::AssistantTextDelta(_)));

        let completed = StreamEvent::ToolExecutionCompleted(ToolExecutionCompleted {
            tool_name: "t".into(),
            output: "o".into(),
            is_error: true,
        });
        assert!(matches!(completed, StreamEvent::ToolExecutionCompleted(_)));
    }
}
