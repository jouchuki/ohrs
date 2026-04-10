//! Conversation message models used by the query engine.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Plain text content block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextBlock {
    #[serde(default = "text_type_tag", skip_serializing)]
    pub r#type: String,
    pub text: String,
}

fn text_type_tag() -> String {
    "text".into()
}

impl TextBlock {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            r#type: "text".into(),
            text: text.into(),
        }
    }
}

/// A request from the model to execute a named tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolUseBlock {
    #[serde(default = "tool_use_type_tag", skip_serializing)]
    pub r#type: String,
    #[serde(default = "generate_tool_id")]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub input: HashMap<String, serde_json::Value>,
}

fn tool_use_type_tag() -> String {
    "tool_use".into()
}

fn generate_tool_id() -> String {
    format!("toolu_{}", Uuid::new_v4().simple())
}

impl ToolUseBlock {
    pub fn new(name: impl Into<String>, input: HashMap<String, serde_json::Value>) -> Self {
        Self {
            r#type: "tool_use".into(),
            id: generate_tool_id(),
            name: name.into(),
            input,
        }
    }
}

/// Tool result content sent back to the model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolResultBlock {
    #[serde(default = "tool_result_type_tag", skip_serializing)]
    pub r#type: String,
    pub tool_use_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

fn tool_result_type_tag() -> String {
    "tool_result".into()
}

impl ToolResultBlock {
    pub fn new(tool_use_id: impl Into<String>, content: impl Into<String>, is_error: bool) -> Self {
        Self {
            r#type: "tool_result".into(),
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error,
        }
    }
}

/// A content block in a conversation message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text(TextBlock),
    #[serde(rename = "tool_use")]
    ToolUse(ToolUseBlock),
    #[serde(rename = "tool_result")]
    ToolResult(ToolResultBlock),
}

/// Message role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// A single assistant or user message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConversationMessage {
    pub role: Role,
    #[serde(default)]
    pub content: Vec<ContentBlock>,
}

impl ConversationMessage {
    /// Construct a user message from raw text.
    pub fn from_user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text(TextBlock::new(text))],
        }
    }

    /// Return concatenated text blocks.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Return all tool calls contained in the message.
    pub fn tool_uses(&self) -> Vec<&ToolUseBlock> {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    /// Convert the message into Anthropic SDK message params.
    pub fn to_api_param(&self) -> serde_json::Value {
        serde_json::json!({
            "role": self.role,
            "content": self.content.iter().map(serialize_content_block).collect::<Vec<_>>(),
        })
    }
}

/// Convert a local content block into the provider wire format.
pub fn serialize_content_block(block: &ContentBlock) -> serde_json::Value {
    match block {
        ContentBlock::Text(t) => serde_json::json!({
            "type": "text",
            "text": t.text,
        }),
        ContentBlock::ToolUse(t) => serde_json::json!({
            "type": "tool_use",
            "id": t.id,
            "name": t.name,
            "input": t.input,
        }),
        ContentBlock::ToolResult(t) => serde_json::json!({
            "type": "tool_result",
            "tool_use_id": t.tool_use_id,
            "content": t.content,
            "is_error": t.is_error,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_block_new() {
        let block = TextBlock::new("hello");
        assert_eq!(block.r#type, "text");
        assert_eq!(block.text, "hello");
    }

    #[test]
    fn test_text_block_serde_roundtrip() {
        let block = TextBlock::new("hello world");
        let json = serde_json::to_string(&block).unwrap();
        let deser: TextBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, deser);
    }

    #[test]
    fn test_tool_use_block_new() {
        let mut input = HashMap::new();
        input.insert("key".to_string(), serde_json::json!("value"));
        let block = ToolUseBlock::new("my_tool", input.clone());
        assert_eq!(block.r#type, "tool_use");
        assert_eq!(block.name, "my_tool");
        assert_eq!(block.input, input);
        assert!(block.id.starts_with("toolu_"));
    }

    #[test]
    fn test_tool_use_block_serde_roundtrip() {
        let block = ToolUseBlock::new("read_file", HashMap::new());
        let json = serde_json::to_string(&block).unwrap();
        let deser: ToolUseBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, deser);
    }

    #[test]
    fn test_tool_result_block_new() {
        let block = ToolResultBlock::new("id_123", "output text", false);
        assert_eq!(block.r#type, "tool_result");
        assert_eq!(block.tool_use_id, "id_123");
        assert_eq!(block.content, "output text");
        assert!(!block.is_error);
    }

    #[test]
    fn test_tool_result_block_error() {
        let block = ToolResultBlock::new("id_456", "failure", true);
        assert!(block.is_error);
    }

    #[test]
    fn test_tool_result_block_serde_roundtrip() {
        let block = ToolResultBlock::new("id_789", "result", false);
        let json = serde_json::to_string(&block).unwrap();
        let deser: ToolResultBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, deser);
    }

    #[test]
    fn test_content_block_text_serde_discriminator() {
        let block = ContentBlock::Text(TextBlock::new("hi"));
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        let deser: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, deser);
    }

    #[test]
    fn test_content_block_tool_use_serde_discriminator() {
        let block = ContentBlock::ToolUse(ToolUseBlock::new("bash", HashMap::new()));
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"tool_use\""));
        let deser: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, deser);
    }

    #[test]
    fn test_content_block_tool_result_serde_discriminator() {
        let block = ContentBlock::ToolResult(ToolResultBlock::new("id", "out", false));
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"tool_result\""));
        let deser: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, deser);
    }

    #[test]
    fn test_role_serde() {
        let json = serde_json::to_string(&Role::User).unwrap();
        assert_eq!(json, "\"user\"");
        let json = serde_json::to_string(&Role::Assistant).unwrap();
        assert_eq!(json, "\"assistant\"");
        let deser: Role = serde_json::from_str("\"user\"").unwrap();
        assert_eq!(deser, Role::User);
    }

    #[test]
    fn test_conversation_message_from_user_text() {
        let msg = ConversationMessage::from_user_text("hello");
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content.len(), 1);
        assert_eq!(msg.text(), "hello");
    }

    #[test]
    fn test_conversation_message_text_concatenation() {
        let msg = ConversationMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text(TextBlock::new("foo")),
                ContentBlock::ToolUse(ToolUseBlock::new("t", HashMap::new())),
                ContentBlock::Text(TextBlock::new("bar")),
            ],
        };
        assert_eq!(msg.text(), "foobar");
    }

    #[test]
    fn test_conversation_message_tool_uses() {
        let msg = ConversationMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text(TextBlock::new("thinking")),
                ContentBlock::ToolUse(ToolUseBlock::new("bash", HashMap::new())),
                ContentBlock::ToolUse(ToolUseBlock::new("read", HashMap::new())),
            ],
        };
        let tools = msg.tool_uses();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "bash");
        assert_eq!(tools[1].name, "read");
    }

    #[test]
    fn test_conversation_message_tool_uses_empty() {
        let msg = ConversationMessage::from_user_text("hi");
        assert!(msg.tool_uses().is_empty());
    }

    #[test]
    fn test_conversation_message_to_api_param() {
        let msg = ConversationMessage::from_user_text("test");
        let param = msg.to_api_param();
        assert_eq!(param["role"], "user");
        assert!(param["content"].is_array());
        assert_eq!(param["content"][0]["type"], "text");
        assert_eq!(param["content"][0]["text"], "test");
    }

    #[test]
    fn test_serialize_content_block_text() {
        let block = ContentBlock::Text(TextBlock::new("abc"));
        let val = serialize_content_block(&block);
        assert_eq!(val["type"], "text");
        assert_eq!(val["text"], "abc");
    }

    #[test]
    fn test_serialize_content_block_tool_result() {
        let block = ContentBlock::ToolResult(ToolResultBlock::new("tid", "out", true));
        let val = serialize_content_block(&block);
        assert_eq!(val["type"], "tool_result");
        assert_eq!(val["tool_use_id"], "tid");
        assert_eq!(val["is_error"], true);
    }

    #[test]
    fn test_conversation_message_serde_roundtrip() {
        let msg = ConversationMessage::from_user_text("roundtrip");
        let json = serde_json::to_string(&msg).unwrap();
        let deser: ConversationMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deser);
    }
}
