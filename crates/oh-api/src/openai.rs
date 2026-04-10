//! OpenAI-compatible API client with streaming and retry.
//!
//! Translates between OpenHarness's internal Anthropic-style message format
//! and the OpenAI Chat Completions API.

use std::pin::Pin;
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use oh_types::api::*;
use oh_types::messages::{
    ContentBlock, ConversationMessage, Role, TextBlock, ToolUseBlock,
};
use opentelemetry::KeyValue;
use tracing::{info_span, warn, Instrument};

use crate::client::StreamingApiClient;

/// OpenAI Chat Completions API client.
pub struct OpenAiApiClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl OpenAiApiClient {
    pub fn new(api_key: impl Into<String>, base_url: Option<&str>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: base_url
                .unwrap_or("https://api.openai.com")
                .trim_end_matches('/')
                .to_string(),
        }
    }

    /// Single attempt at calling the Chat Completions endpoint.
    async fn complete_once(
        &self,
        request: &ApiMessageRequest,
    ) -> Result<(ConversationMessage, UsageSnapshot, Option<String>), ApiError> {
        let span = info_span!("openai_api_request", model = %request.model);

        async {
            let start = Instant::now();

            // Build the messages array in OpenAI format.
            let mut oai_messages: Vec<serde_json::Value> = Vec::new();

            // System prompt → system message
            if let Some(ref sys) = request.system_prompt {
                oai_messages.push(serde_json::json!({
                    "role": "system",
                    "content": sys,
                }));
            }

            // Convert each internal message
            for msg in &request.messages {
                convert_message_to_openai(msg, &mut oai_messages);
            }

            let mut body = serde_json::json!({
                "model": request.model,
                "max_completion_tokens": request.max_tokens,
                "messages": oai_messages,
            });

            // Convert tools from Anthropic format to OpenAI function-calling format
            if !request.tools.is_empty() {
                let oai_tools: Vec<serde_json::Value> = request
                    .tools
                    .iter()
                    .map(convert_tool_to_openai)
                    .collect();
                body["tools"] = serde_json::json!(oai_tools);
            }

            let response = self
                .http
                .post(format!("{}/v1/chat/completions", self.base_url))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| ApiError::Network(e.to_string()))?;

            let status = response.status().as_u16();
            if !response.status().is_success() {
                let text = response.text().await.unwrap_or_default();
                if status == 401 || status == 403 {
                    return Err(ApiError::Authentication(text));
                }
                if status == 429 {
                    return Err(ApiError::RateLimit(text));
                }
                return Err(ApiError::Request(format!("status {status}: {text}")));
            }

            let resp_text = response
                .text()
                .await
                .map_err(|e| ApiError::Network(e.to_string()))?;

            let resp_json: serde_json::Value = serde_json::from_str(&resp_text)
                .map_err(|e| ApiError::Request(format!("invalid JSON: {e}")))?;

            let (message, usage, stop_reason) = parse_openai_response(&resp_json)?;

            let elapsed = start.elapsed().as_secs_f64();
            oh_telemetry::API_REQUEST_DURATION.record(
                elapsed,
                &[
                    KeyValue::new("model", request.model.clone()),
                    KeyValue::new("status", "ok"),
                ],
            );
            oh_telemetry::TOKEN_USAGE_TOTAL.add(
                usage.input_tokens,
                &[
                    KeyValue::new("model", request.model.clone()),
                    KeyValue::new("direction", "input"),
                ],
            );
            oh_telemetry::TOKEN_USAGE_TOTAL.add(
                usage.output_tokens,
                &[
                    KeyValue::new("model", request.model.clone()),
                    KeyValue::new("direction", "output"),
                ],
            );

            Ok((message, usage, stop_reason))
        }
        .instrument(span)
        .await
    }
}

#[async_trait]
impl StreamingApiClient for OpenAiApiClient {
    async fn stream_message(
        &self,
        request: ApiMessageRequest,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ApiStreamEvent, ApiError>> + Send + '_>>,
        ApiError,
    > {
        let mut last_error: Option<ApiError> = None;

        for attempt in 0..=MAX_RETRIES {
            match self.complete_once(&request).await {
                Ok((message, usage, stop_reason)) => {
                    let events = vec![Ok(ApiStreamEvent::MessageComplete(
                        ApiMessageCompleteEvent {
                            message,
                            usage,
                            stop_reason,
                        },
                    ))];
                    return Ok(Box::pin(futures::stream::iter(events)));
                }
                Err(e) => {
                    if attempt >= MAX_RETRIES || !is_retryable(&e) {
                        return Err(e);
                    }
                    let delay = get_retry_delay(attempt);
                    warn!(
                        attempt = attempt + 1,
                        max_retries = MAX_RETRIES + 1,
                        delay_secs = delay,
                        "OpenAI API request failed, retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs_f64(delay)).await;
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or(ApiError::Request("unknown error".into())))
    }
}

// ── Format conversion helpers ──────────────────────────────────────────

/// Convert an internal `ConversationMessage` into one or more OpenAI messages.
///
/// Anthropic packs tool results as content blocks inside a user message.
/// OpenAI expects separate `{ role: "tool", tool_call_id, content }` messages.
fn convert_message_to_openai(
    msg: &ConversationMessage,
    out: &mut Vec<serde_json::Value>,
) {
    match msg.role {
        Role::User => {
            // Separate text blocks from tool-result blocks.
            let mut texts: Vec<String> = Vec::new();
            let mut tool_results: Vec<&oh_types::messages::ToolResultBlock> = Vec::new();

            for block in &msg.content {
                match block {
                    ContentBlock::Text(t) => texts.push(t.text.clone()),
                    ContentBlock::ToolResult(tr) => tool_results.push(tr),
                    _ => {}
                }
            }

            // Emit tool-result messages first (OpenAI expects them right after
            // the assistant message that produced the tool_calls).
            for tr in &tool_results {
                out.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tr.tool_use_id,
                    "content": tr.content,
                }));
            }

            // If there's any user text, emit a user message.
            if !texts.is_empty() {
                out.push(serde_json::json!({
                    "role": "user",
                    "content": texts.join(""),
                }));
            }
        }
        Role::Assistant => {
            let mut text_parts: Vec<String> = Vec::new();
            let mut tool_calls: Vec<serde_json::Value> = Vec::new();

            for block in &msg.content {
                match block {
                    ContentBlock::Text(t) => text_parts.push(t.text.clone()),
                    ContentBlock::ToolUse(tu) => {
                        tool_calls.push(serde_json::json!({
                            "id": tu.id,
                            "type": "function",
                            "function": {
                                "name": tu.name,
                                "arguments": serde_json::to_string(&tu.input).unwrap_or_default(),
                            }
                        }));
                    }
                    _ => {}
                }
            }

            let content_str = texts_to_content(&text_parts);
            let mut assistant_msg = serde_json::json!({
                "role": "assistant",
                "content": content_str,
            });
            if !tool_calls.is_empty() {
                assistant_msg["tool_calls"] = serde_json::json!(tool_calls);
            }
            out.push(assistant_msg);
        }
    }
}

fn texts_to_content(texts: &[String]) -> serde_json::Value {
    let joined = texts.join("");
    if joined.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(joined)
    }
}

/// Convert an Anthropic-format tool definition to OpenAI function-calling format.
///
/// Anthropic: `{ name, description, input_schema: { ... } }`
/// OpenAI:    `{ type: "function", function: { name, description, parameters: { ... } } }`
fn convert_tool_to_openai(tool: &serde_json::Value) -> serde_json::Value {
    // If it's already in OpenAI format, pass through.
    if tool.get("type").and_then(|v| v.as_str()) == Some("function") {
        return tool.clone();
    }

    let name = tool.get("name").cloned().unwrap_or(serde_json::json!(""));
    let description = tool.get("description").cloned().unwrap_or(serde_json::json!(""));
    let parameters = tool
        .get("input_schema")
        .cloned()
        .unwrap_or(serde_json::json!({"type": "object", "properties": {}}));

    serde_json::json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters,
        }
    })
}

/// Parse an OpenAI Chat Completions JSON response into internal types.
fn parse_openai_response(
    resp: &serde_json::Value,
) -> Result<(ConversationMessage, UsageSnapshot, Option<String>), ApiError> {
    let choice = resp
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or_else(|| ApiError::Request("no choices in response".into()))?;

    let oai_msg = choice
        .get("message")
        .ok_or_else(|| ApiError::Request("no message in choice".into()))?;

    let mut content_blocks: Vec<ContentBlock> = Vec::new();

    // Text content
    if let Some(text) = oai_msg.get("content").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            content_blocks.push(ContentBlock::Text(TextBlock::new(text)));
        }
    }

    // Tool calls
    if let Some(tool_calls) = oai_msg.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            let id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let func = tc.get("function").unwrap_or(&serde_json::Value::Null);
            let name = func
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args_str = func
                .get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let input: std::collections::HashMap<String, serde_json::Value> =
                serde_json::from_str(args_str).unwrap_or_default();

            content_blocks.push(ContentBlock::ToolUse(ToolUseBlock {
                r#type: "tool_use".into(),
                id,
                name,
                input,
            }));
        }
    }

    let message = ConversationMessage {
        role: Role::Assistant,
        content: content_blocks,
    };

    // Usage
    let usage_obj = resp.get("usage");
    let usage = UsageSnapshot {
        input_tokens: usage_obj
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        output_tokens: usage_obj
            .and_then(|u| u.get("completion_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        ..Default::default()
    };

    // Stop reason (finish_reason → stop_reason)
    let stop_reason = choice
        .get("finish_reason")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "stop" => "end_turn".to_string(),
            "tool_calls" => "tool_use".to_string(),
            "length" => "max_tokens".to_string(),
            other => other.to_string(),
        });

    Ok((message, usage, stop_reason))
}

fn is_retryable(err: &ApiError) -> bool {
    matches!(err, ApiError::RateLimit(_) | ApiError::Network(_))
        || matches!(err, ApiError::Request(msg) if {
            RETRYABLE_STATUS_CODES.iter().any(|code| msg.contains(&code.to_string()))
        })
}

fn get_retry_delay(attempt: u32) -> f64 {
    let delay = (BASE_DELAY_SECS * 2.0_f64.powi(attempt as i32)).min(MAX_DELAY_SECS);
    let jitter = delay * 0.25 * rand_fraction();
    delay + jitter
}

fn rand_fraction() -> f64 {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos as f64) / (u32::MAX as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ── parse_openai_response ───────────────────────────────────────

    #[test]
    fn test_parse_openai_response_simple_text() {
        let resp = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello, world!"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });

        let (msg, usage, stop) = parse_openai_response(&resp).unwrap();
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content.len(), 1);
        match &msg.content[0] {
            ContentBlock::Text(tb) => assert_eq!(tb.text, "Hello, world!"),
            other => panic!("expected Text block, got {:?}", other),
        }
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(stop.as_deref(), Some("end_turn"));
    }

    #[test]
    fn test_parse_openai_response_tool_call() {
        let resp = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc123",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"/tmp/a.txt\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 20,
                "completion_tokens": 10,
                "total_tokens": 30
            }
        });

        let (msg, _usage, stop) = parse_openai_response(&resp).unwrap();
        assert_eq!(msg.content.len(), 1);
        match &msg.content[0] {
            ContentBlock::ToolUse(tu) => {
                assert_eq!(tu.id, "call_abc123");
                assert_eq!(tu.name, "read_file");
                assert_eq!(
                    tu.input.get("path").and_then(|v| v.as_str()),
                    Some("/tmp/a.txt"),
                );
            }
            other => panic!("expected ToolUse block, got {:?}", other),
        }
        assert_eq!(stop.as_deref(), Some("tool_use"));
    }

    #[test]
    fn test_parse_openai_response_text_with_tool_calls() {
        let resp = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Let me check.",
                    "tool_calls": [{
                        "id": "call_xyz",
                        "type": "function",
                        "function": {
                            "name": "bash",
                            "arguments": "{\"cmd\":\"ls\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 15,
                "completion_tokens": 8,
                "total_tokens": 23
            }
        });

        let (msg, _usage, _stop) = parse_openai_response(&resp).unwrap();
        assert_eq!(msg.content.len(), 2);
        match &msg.content[0] {
            ContentBlock::Text(tb) => assert_eq!(tb.text, "Let me check."),
            other => panic!("expected Text block, got {:?}", other),
        }
        match &msg.content[1] {
            ContentBlock::ToolUse(tu) => assert_eq!(tu.name, "bash"),
            other => panic!("expected ToolUse block, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_openai_response_no_choices() {
        let resp = serde_json::json!({ "choices": [] });
        assert!(parse_openai_response(&resp).is_err());
    }

    #[test]
    fn test_parse_openai_response_missing_usage() {
        let resp = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "hi" },
                "finish_reason": "stop"
            }]
        });
        let (_msg, usage, _stop) = parse_openai_response(&resp).unwrap();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
    }

    #[test]
    fn test_parse_openai_response_length_stop_reason() {
        let resp = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "truncated" },
                "finish_reason": "length"
            }],
            "usage": { "prompt_tokens": 0, "completion_tokens": 0 }
        });
        let (_msg, _usage, stop) = parse_openai_response(&resp).unwrap();
        assert_eq!(stop.as_deref(), Some("max_tokens"));
    }

    // ── convert_message_to_openai ──────────────────────────────────

    #[test]
    fn test_convert_user_text_message() {
        let msg = ConversationMessage::from_user_text("hello");
        let mut out = Vec::new();
        convert_message_to_openai(&msg, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["content"], "hello");
    }

    #[test]
    fn test_convert_assistant_text_message() {
        let msg = ConversationMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Text(TextBlock::new("thinking out loud"))],
        };
        let mut out = Vec::new();
        convert_message_to_openai(&msg, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "assistant");
        assert_eq!(out[0]["content"], "thinking out loud");
        assert!(out[0].get("tool_calls").is_none());
    }

    #[test]
    fn test_convert_assistant_with_tool_use() {
        let msg = ConversationMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text(TextBlock::new("I'll read that.")),
                ContentBlock::ToolUse(ToolUseBlock {
                    r#type: "tool_use".into(),
                    id: "tu_1".into(),
                    name: "read_file".into(),
                    input: {
                        let mut m = HashMap::new();
                        m.insert("path".into(), serde_json::json!("/tmp/a.txt"));
                        m
                    },
                }),
            ],
        };
        let mut out = Vec::new();
        convert_message_to_openai(&msg, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "assistant");
        assert_eq!(out[0]["content"], "I'll read that.");
        let tc = &out[0]["tool_calls"];
        assert!(tc.is_array());
        assert_eq!(tc[0]["id"], "tu_1");
        assert_eq!(tc[0]["function"]["name"], "read_file");
    }

    #[test]
    fn test_convert_user_tool_result_message() {
        let msg = ConversationMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult(
                oh_types::messages::ToolResultBlock::new("tu_1", "file contents here", false),
            )],
        };
        let mut out = Vec::new();
        convert_message_to_openai(&msg, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "tool");
        assert_eq!(out[0]["tool_call_id"], "tu_1");
        assert_eq!(out[0]["content"], "file contents here");
    }

    #[test]
    fn test_convert_user_mixed_tool_result_and_text() {
        let msg = ConversationMessage {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult(oh_types::messages::ToolResultBlock::new(
                    "tu_1", "result", false,
                )),
                ContentBlock::Text(TextBlock::new("follow-up question")),
            ],
        };
        let mut out = Vec::new();
        convert_message_to_openai(&msg, &mut out);
        // Tool result first, then user text
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["role"], "tool");
        assert_eq!(out[1]["role"], "user");
        assert_eq!(out[1]["content"], "follow-up question");
    }

    // ── convert_tool_to_openai ─────────────────────────────────────

    #[test]
    fn test_convert_anthropic_tool_to_openai() {
        let tool = serde_json::json!({
            "name": "read_file",
            "description": "Read a file from disk",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" }
                },
                "required": ["path"]
            }
        });
        let converted = convert_tool_to_openai(&tool);
        assert_eq!(converted["type"], "function");
        assert_eq!(converted["function"]["name"], "read_file");
        assert_eq!(converted["function"]["description"], "Read a file from disk");
        assert_eq!(converted["function"]["parameters"]["type"], "object");
        assert!(converted["function"]["parameters"]["properties"]["path"].is_object());
    }

    #[test]
    fn test_convert_already_openai_format_passthrough() {
        let tool = serde_json::json!({
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Run a command",
                "parameters": { "type": "object", "properties": {} }
            }
        });
        let converted = convert_tool_to_openai(&tool);
        assert_eq!(converted, tool);
    }

    // ── OpenAiApiClient::new ───────────────────────────────────────

    #[test]
    fn test_openai_client_new_default_url() {
        let client = OpenAiApiClient::new("sk-test", None);
        assert_eq!(client.base_url, "https://api.openai.com");
        assert_eq!(client.api_key, "sk-test");
    }

    #[test]
    fn test_openai_client_new_custom_url() {
        let client = OpenAiApiClient::new("sk-test", Some("https://custom.openai.example.com/"));
        assert_eq!(client.base_url, "https://custom.openai.example.com");
    }

    // ── retry helpers ──────────────────────────────────────────────

    #[test]
    fn test_is_retryable_rate_limit() {
        assert!(is_retryable(&ApiError::RateLimit("slow down".into())));
    }

    #[test]
    fn test_is_retryable_network() {
        assert!(is_retryable(&ApiError::Network("timeout".into())));
    }

    #[test]
    fn test_is_retryable_authentication_false() {
        assert!(!is_retryable(&ApiError::Authentication("bad key".into())));
    }
}
