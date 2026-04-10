//! Ask the user a question tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct AskUserQuestionTool;

#[async_trait]
impl crate::traits::Tool for AskUserQuestionTool {
    fn name(&self) -> &str {
        "AskUserQuestion"
    }

    fn description(&self) -> &str {
        "Ask the interactive user a follow-up question and return the answer."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "Array of question objects",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": {
                                "type": "string",
                                "description": "The question text"
                            },
                            "header": {
                                "type": "string",
                                "description": "Optional header for the question"
                            },
                            "options": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Optional list of answer options"
                            },
                            "multiSelect": {
                                "type": "boolean",
                                "description": "Whether multiple options can be selected"
                            }
                        },
                        "required": ["question"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let questions = match arguments.get("questions").and_then(|v| v.as_array()) {
            Some(q) => q,
            None => return ToolResult::error("Missing required parameter: questions"),
        };

        if questions.is_empty() {
            return ToolResult::error("questions array must not be empty");
        }

        if questions[0].get("question").and_then(|v| v.as_str()).is_none() {
            return ToolResult::error("First question object must have a 'question' field");
        }

        // If the metadata contains an "ask_user_prompt" key, treat it as the
        // pre-recorded answer (useful for testing / scripted runs). In a full
        // implementation this would be a function pointer or channel.
        if let Some(answer) = context.metadata.get("ask_user_prompt") {
            if let Some(ans) = answer.as_str() {
                let trimmed = ans.trim();
                if trimmed.is_empty() {
                    return ToolResult::success("(no response)");
                }
                return ToolResult::success(trimmed);
            }
        }

        // Non-interactive mode — no user prompt callback available.
        ToolResult::success(
            "No user prompt available — running in non-interactive mode.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext::new(std::env::current_dir().unwrap())
    }

    #[test]
    fn test_schema_has_required_questions() {
        let tool = AskUserQuestionTool;
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "questions"));
    }

    #[test]
    fn test_is_read_only_returns_true() {
        let tool = AskUserQuestionTool;
        assert!(tool.is_read_only(&serde_json::json!({})));
    }

    #[tokio::test]
    async fn test_non_interactive_mode() {
        let tool = AskUserQuestionTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "questions": [{"question": "What is your name?"}]
                }),
                &ctx(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("non-interactive"));
    }

    #[tokio::test]
    async fn test_missing_questions() {
        let tool = AskUserQuestionTool;
        let result = tool.execute(serde_json::json!({}), &ctx()).await;
        assert!(result.is_error);
        assert!(result.output.contains("questions"));
    }

    #[tokio::test]
    async fn test_empty_questions_array() {
        let tool = AskUserQuestionTool;
        let result = tool
            .execute(serde_json::json!({"questions": []}), &ctx())
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("empty"));
    }

    #[tokio::test]
    async fn test_with_ask_user_prompt_in_metadata() {
        let tool = AskUserQuestionTool;
        let mut context = ctx();
        context.metadata.insert(
            "ask_user_prompt".to_string(),
            serde_json::json!("Alice"),
        );
        let result = tool
            .execute(
                serde_json::json!({
                    "questions": [{"question": "What is your name?"}]
                }),
                &context,
            )
            .await;
        assert!(!result.is_error);
        assert_eq!(result.output, "Alice");
    }
}
