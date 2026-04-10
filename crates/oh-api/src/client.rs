//! Anthropic API client with streaming and retry.

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

/// Protocol for streaming API messages — used in tests and production.
#[async_trait]
pub trait StreamingApiClient: Send + Sync {
    async fn stream_message(
        &self,
        request: ApiMessageRequest,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ApiStreamEvent, ApiError>> + Send + '_>>,
        ApiError,
    >;
}

/// Thin wrapper around the Anthropic HTTP API with retry logic.
pub struct AnthropicApiClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl AnthropicApiClient {
    pub fn new(api_key: impl Into<String>, base_url: Option<&str>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: base_url
                .unwrap_or("https://api.anthropic.com")
                .to_string(),
        }
    }

    /// Single attempt at streaming a message via the Anthropic messages API.
    async fn stream_once(
        &self,
        request: &ApiMessageRequest,
    ) -> Result<(ConversationMessage, UsageSnapshot, Option<String>), ApiError> {
        let span = info_span!("api_request", model = %request.model);

        async {
            let start = Instant::now();

            let mut body = serde_json::json!({
                "model": request.model,
                "max_tokens": request.max_tokens,
                "messages": request.messages.iter().map(|m| m.to_api_param()).collect::<Vec<_>>(),
                "stream": true,
            });

            if let Some(ref sys) = request.system_prompt {
                // Use cache_control to enable prompt caching on the system prompt
                body["system"] = serde_json::json!([{
                    "type": "text",
                    "text": sys,
                    "cache_control": {"type": "ephemeral"}
                }]);
            }
            if !request.tools.is_empty() {
                body["tools"] = serde_json::json!(request.tools);
            }

            let response = self
                .http
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
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

            // Parse SSE stream
            let text = response.text().await.map_err(|e| ApiError::Network(e.to_string()))?;
            let (message, usage, stop_reason) = parse_sse_response(&text)?;

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
impl StreamingApiClient for AnthropicApiClient {
    async fn stream_message(
        &self,
        request: ApiMessageRequest,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ApiStreamEvent, ApiError>> + Send + '_>>,
        ApiError,
    > {
        let mut last_error: Option<ApiError> = None;

        for attempt in 0..=MAX_RETRIES {
            match self.stream_once(&request).await {
                Ok((message, usage, stop_reason)) => {
                    // Return a stream that yields the complete event
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
                        "API request failed, retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs_f64(delay)).await;
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or(ApiError::Request("unknown error".into())))
    }
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
    // Simple deterministic pseudo-random for jitter
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos as f64) / (u32::MAX as f64)
}

/// Parse an SSE response body into a conversation message.
fn parse_sse_response(
    body: &str,
) -> Result<(ConversationMessage, UsageSnapshot, Option<String>), ApiError> {
    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    let mut current_text = String::new();
    let mut usage = UsageSnapshot::default();
    let mut stop_reason: Option<String> = None;

    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with("data: ") {
            continue;
        }
        let data = &line[6..];
        if data == "[DONE]" {
            break;
        }

        let event: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = event["type"].as_str().unwrap_or("");

        match event_type {
            "content_block_start" => {
                let block = &event["content_block"];
                if block["type"].as_str() == Some("tool_use") {
                    // Flush accumulated text
                    if !current_text.is_empty() {
                        content_blocks.push(ContentBlock::Text(TextBlock::new(
                            std::mem::take(&mut current_text),
                        )));
                    }
                    content_blocks.push(ContentBlock::ToolUse(ToolUseBlock {
                        r#type: "tool_use".into(),
                        id: block["id"].as_str().unwrap_or("").into(),
                        name: block["name"].as_str().unwrap_or("").into(),
                        input: Default::default(),
                    }));
                }
            }
            "content_block_delta" => {
                let delta = &event["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        if let Some(text) = delta["text"].as_str() {
                            current_text.push_str(text);
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(json_str) = delta["partial_json"].as_str() {
                            // Accumulate JSON for the last tool_use block
                            if let Some(ContentBlock::ToolUse(ref mut tu)) =
                                content_blocks.last_mut()
                            {
                                // We'll parse the complete JSON at content_block_stop
                                let existing = tu
                                    .input
                                    .entry("__partial".into())
                                    .or_insert(serde_json::Value::String(String::new()));
                                if let serde_json::Value::String(ref mut s) = existing {
                                    s.push_str(json_str);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                // Finalize any partial JSON in tool_use blocks
                if let Some(ContentBlock::ToolUse(ref mut tu)) = content_blocks.last_mut() {
                    if let Some(serde_json::Value::String(partial)) =
                        tu.input.remove("__partial")
                    {
                        if let Ok(parsed) = serde_json::from_str::<
                            std::collections::HashMap<String, serde_json::Value>,
                        >(&partial)
                        {
                            tu.input = parsed;
                        }
                    }
                }
            }
            "message_delta" => {
                if let Some(sr) = event["delta"]["stop_reason"].as_str() {
                    stop_reason = Some(sr.to_string());
                }
                // message_delta usage is CUMULATIVE and is the source of truth
                if let Some(u) = event["usage"].as_object() {
                    if let Some(v) = u.get("output_tokens").and_then(|v| v.as_u64()) {
                        usage.output_tokens = v;
                    }
                    if let Some(v) = u.get("input_tokens").and_then(|v| v.as_u64()) {
                        usage.input_tokens = v;
                    }
                    if let Some(v) = u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()) {
                        usage.cache_creation_input_tokens = v;
                    }
                    if let Some(v) = u.get("cache_read_input_tokens").and_then(|v| v.as_u64()) {
                        usage.cache_read_input_tokens = v;
                    }
                }
            }
            "message_start" => {
                // Preliminary counts — will be overwritten by message_delta
                if let Some(u) = event["message"]["usage"].as_object() {
                    if let Some(v) = u.get("input_tokens").and_then(|v| v.as_u64()) {
                        usage.input_tokens = v;
                    }
                    if let Some(v) = u.get("output_tokens").and_then(|v| v.as_u64()) {
                        usage.output_tokens = v;
                    }
                    if let Some(v) = u.get("cache_creation_input_tokens").and_then(|v| v.as_u64()) {
                        usage.cache_creation_input_tokens = v;
                    }
                    if let Some(v) = u.get("cache_read_input_tokens").and_then(|v| v.as_u64()) {
                        usage.cache_read_input_tokens = v;
                    }
                }
            }
            _ => {}
        }
    }

    // Flush remaining text
    if !current_text.is_empty() {
        content_blocks.push(ContentBlock::Text(TextBlock::new(current_text)));
    }

    let message = ConversationMessage {
        role: Role::Assistant,
        content: content_blocks,
    };

    Ok((message, usage, stop_reason))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_sse_response ──────────────────────────────────────────

    #[test]
    fn test_parse_sse_response_simple_text() {
        let sse = "\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello, world!\"}}\n\
data: [DONE]\n";

        let (msg, _usage, _stop) = parse_sse_response(sse).unwrap();
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content.len(), 1);
        match &msg.content[0] {
            ContentBlock::Text(tb) => assert_eq!(tb.text, "Hello, world!"),
            other => panic!("expected Text block, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_sse_response_tool_use() {
        let sse = "\
data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read_file\"}}\n\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"/tmp/a.txt\\\"}\"}}\n\
data: {\"type\":\"content_block_stop\"}\n\
data: [DONE]\n";

        let (msg, _usage, _stop) = parse_sse_response(sse).unwrap();
        assert_eq!(msg.content.len(), 1);
        match &msg.content[0] {
            ContentBlock::ToolUse(tu) => {
                assert_eq!(tu.id, "tu_1");
                assert_eq!(tu.name, "read_file");
                assert_eq!(
                    tu.input.get("path").and_then(|v| v.as_str()),
                    Some("/tmp/a.txt"),
                );
            }
            other => panic!("expected ToolUse block, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_sse_response_text_then_tool_use() {
        let sse = "\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me check.\"}}\n\
data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_2\",\"name\":\"bash\"}}\n\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}\n\
data: {\"type\":\"content_block_stop\"}\n\
data: [DONE]\n";

        let (msg, _usage, _stop) = parse_sse_response(sse).unwrap();
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
    fn test_parse_sse_response_message_start_extracts_input_tokens() {
        let sse = "\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":42}}}\n\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\
data: [DONE]\n";

        let (_msg, usage, _stop) = parse_sse_response(sse).unwrap();
        assert_eq!(usage.input_tokens, 42);
    }

    #[test]
    fn test_parse_sse_response_message_delta_extracts_output_tokens_and_stop_reason() {
        let sse = "\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":17}}\n\
data: [DONE]\n";

        let (_msg, usage, stop) = parse_sse_response(sse).unwrap();
        assert_eq!(usage.output_tokens, 17);
        assert_eq!(stop.as_deref(), Some("end_turn"));
    }

    #[test]
    fn test_parse_sse_response_empty_body() {
        let (msg, usage, stop) = parse_sse_response("").unwrap();
        assert_eq!(msg.role, Role::Assistant);
        assert!(msg.content.is_empty());
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert!(stop.is_none());
    }

    #[test]
    fn test_parse_sse_response_ignores_non_data_lines() {
        let sse = "\
event: ping\n\
: comment\n\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\
data: [DONE]\n";

        let (msg, _usage, _stop) = parse_sse_response(sse).unwrap();
        assert_eq!(msg.content.len(), 1);
    }

    // ── is_retryable ────────────────────────────────────────────────

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

    #[test]
    fn test_is_retryable_request_with_retryable_status() {
        assert!(is_retryable(&ApiError::Request("status 529: overloaded".into())));
    }

    #[test]
    fn test_is_retryable_request_with_non_retryable_status() {
        assert!(!is_retryable(&ApiError::Request("status 400: bad request".into())));
    }

    // ── get_retry_delay ─────────────────────────────────────────────

    #[test]
    fn test_get_retry_delay_increases_with_attempt() {
        let d0 = get_retry_delay(0);
        let d1 = get_retry_delay(1);
        let d2 = get_retry_delay(2);
        // Base delay is 1.0s; each attempt doubles the base.
        // With up to 25% jitter, attempt 0 is in [1.0, 1.25],
        // attempt 1 is in [2.0, 2.5], attempt 2 is in [4.0, 5.0].
        assert!(d0 >= 1.0 && d0 <= 1.25, "d0 = {d0}");
        assert!(d1 >= 2.0 && d1 <= 2.5, "d1 = {d1}");
        assert!(d2 >= 4.0 && d2 <= 5.0, "d2 = {d2}");
        assert!(d1 > d0);
        assert!(d2 > d1);
    }

    #[test]
    fn test_get_retry_delay_capped_at_max() {
        // attempt 10 → 2^10 = 1024, capped at MAX_DELAY_SECS (30)
        let d = get_retry_delay(10);
        assert!(d >= MAX_DELAY_SECS && d <= MAX_DELAY_SECS * 1.25, "d = {d}");
    }

    // ── AnthropicApiClient::new ─────────────────────────────────────

    #[test]
    fn test_anthropic_api_client_new_default_url() {
        let client = AnthropicApiClient::new("sk-test", None);
        assert_eq!(client.base_url, "https://api.anthropic.com");
        assert_eq!(client.api_key, "sk-test");
    }

    #[test]
    fn test_anthropic_api_client_new_custom_url() {
        let client = AnthropicApiClient::new("sk-test", Some("https://custom.example.com"));
        assert_eq!(client.base_url, "https://custom.example.com");
    }
}
