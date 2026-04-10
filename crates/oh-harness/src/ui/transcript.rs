//! Transcript items for the conversation view.

use std::collections::HashMap;

/// A single item in the conversation transcript.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    pub role: TranscriptRole,
    pub text: String,
    pub tool_name: Option<String>,
    pub tool_input: Option<HashMap<String, serde_json::Value>>,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptRole {
    User,
    Assistant,
    Tool,
    ToolResult,
    System,
    Log,
}

impl TranscriptEntry {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: TranscriptRole::User,
            text: text.into(),
            tool_name: None,
            tool_input: None,
            is_error: false,
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: TranscriptRole::Assistant,
            text: text.into(),
            tool_name: None,
            tool_input: None,
            is_error: false,
        }
    }

    pub fn tool_start(name: impl Into<String>, input: HashMap<String, serde_json::Value>) -> Self {
        let name = name.into();
        let summary = tool_summary(&name, &input);
        Self {
            role: TranscriptRole::Tool,
            text: summary,
            tool_name: Some(name),
            tool_input: Some(input),
            is_error: false,
        }
    }

    pub fn tool_result(name: impl Into<String>, output: impl Into<String>, is_error: bool) -> Self {
        Self {
            role: TranscriptRole::ToolResult,
            text: output.into(),
            tool_name: Some(name.into()),
            tool_input: None,
            is_error,
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: TranscriptRole::System,
            text: text.into(),
            tool_name: None,
            tool_input: None,
            is_error: false,
        }
    }
}

/// Generate a short summary for a tool call.
fn tool_summary(name: &str, input: &HashMap<String, serde_json::Value>) -> String {
    match name {
        "Bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| truncate(s, 80))
            .unwrap_or_default(),
        "Read" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Write" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Edit" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Glob" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Grep" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "WebFetch" => input
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| truncate(s, 60))
            .unwrap_or_default(),
        "WebSearch" => input
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}
