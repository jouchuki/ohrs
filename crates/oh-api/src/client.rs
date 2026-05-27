//! Anthropic API client with streaming and retry.

use std::pin::Pin;
use std::time::Instant;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use oh_types::api::*;
use oh_types::messages::{ContentBlock, ConversationMessage, Role, TextBlock, ToolUseBlock};
use opentelemetry::KeyValue;
use tracing::{info_span, warn, Instrument};

/// Protocol for streaming API messages — used in tests and production.
#[async_trait]
pub trait StreamingApiClient: Send + Sync {
    async fn stream_message(
        &self,
        request: ApiMessageRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ApiStreamEvent, ApiError>> + Send + '_>>, ApiError>;
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
            base_url: base_url.unwrap_or("https://api.anthropic.com").to_string(),
        }
    }

    /// Single attempt at streaming a message via the Anthropic messages API.
    ///
    /// Returns the ordered list of incremental [`ApiStreamEvent`]s — zero or
    /// more `TextDelta` / `ToolUseDelta` followed by exactly one
    /// `MessageComplete` — so the caller can surface deltas to the UI as they
    /// arrive rather than buffering the whole response.
    async fn stream_once(
        &self,
        request: &ApiMessageRequest,
    ) -> Result<Vec<Result<ApiStreamEvent, ApiError>>, ApiError> {
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
                let resp_body = response.text().await.unwrap_or_default();
                return Err(map_http_error(status, resp_body));
            }

            // Stream the SSE body incrementally rather than buffering the whole
            // response into a single String.
            let byte_stream = response
                .bytes_stream()
                .map(|chunk| chunk.map_err(|e| e.to_string()));
            let events = parse_sse_stream(byte_stream).await?;

            // Record telemetry from the terminal MessageComplete usage.
            let elapsed = start.elapsed().as_secs_f64();
            oh_telemetry::API_REQUEST_DURATION.record(
                elapsed,
                &[
                    KeyValue::new("model", request.model.clone()),
                    KeyValue::new("status", "ok"),
                ],
            );
            if let Some(Ok(ApiStreamEvent::MessageComplete(complete))) = events
                .iter()
                .rev()
                .find(|e| matches!(e, Ok(ApiStreamEvent::MessageComplete(_))))
            {
                oh_telemetry::TOKEN_USAGE_TOTAL.add(
                    complete.usage.input_tokens,
                    &[
                        KeyValue::new("model", request.model.clone()),
                        KeyValue::new("direction", "input"),
                    ],
                );
                oh_telemetry::TOKEN_USAGE_TOTAL.add(
                    complete.usage.output_tokens,
                    &[
                        KeyValue::new("model", request.model.clone()),
                        KeyValue::new("direction", "output"),
                    ],
                );
            }

            Ok(events)
        }
        .instrument(span)
        .await
    }
}

/// Map a non-success HTTP response to the appropriate [`ApiError`].
///
/// 401/403 and 429 keep their semantically-rich variants (consumed by callers
/// that branch on auth vs. rate-limit); everything else becomes a structured
/// [`ApiError::Http`] carrying the numeric status so retryability is decided
/// on the integer, never on substring-matching the body (ENG-6 / contract C5).
fn map_http_error(status: u16, body: String) -> ApiError {
    match status {
        HTTP_UNAUTHORIZED | HTTP_FORBIDDEN => ApiError::Authentication(body),
        HTTP_TOO_MANY_REQUESTS => ApiError::RateLimit(body),
        _ => ApiError::Http { status, body },
    }
}

#[async_trait]
impl StreamingApiClient for AnthropicApiClient {
    async fn stream_message(
        &self,
        request: ApiMessageRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ApiStreamEvent, ApiError>> + Send + '_>>, ApiError>
    {
        let mut last_error: Option<ApiError> = None;

        for attempt in 0..=MAX_RETRIES {
            match self.stream_once(&request).await {
                Ok(events) => {
                    // `events` already carries the incremental TextDelta /
                    // ToolUseDelta chunks followed by the terminal
                    // MessageComplete, in arrival order.
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

/// Decide retryability on the structured error.
///
/// For [`ApiError::Http`] the decision is made on the numeric `status`, never
/// on substring-matching the human-readable body (ENG-6 / contract C5).
fn is_retryable(err: &ApiError) -> bool {
    match err {
        ApiError::RateLimit(_) | ApiError::Network(_) => true,
        ApiError::Http { status, .. } => RETRYABLE_STATUS_CODES.contains(status),
        ApiError::Authentication(_) | ApiError::Request(_) => false,
    }
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

/// A content block under construction while consuming the SSE stream.
///
/// Anthropic streams content as indexed blocks: a `content_block_start` opens a
/// block at some `index`, zero or more `content_block_delta`s append to it, and
/// a `content_block_stop` closes it. We accumulate per index, preserving the
/// arrival order in which blocks were opened, and finalize into `ContentBlock`s.
enum PendingBlock {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        /// Raw concatenated `partial_json` chunks, parsed once at finalize.
        partial_json: String,
    },
}

/// Incremental state accumulated while consuming an Anthropic messages SSE stream.
#[derive(Default)]
struct AnthropicSseAccum {
    /// Blocks in the order they were opened (`content_block_start.index`).
    block_order: Vec<usize>,
    blocks: std::collections::HashMap<usize, PendingBlock>,
    usage: UsageSnapshot,
    stop_reason: Option<String>,
}

/// Drive an Anthropic messages SSE byte stream, emitting incremental
/// `TextDelta` / `ToolUseDelta` events followed by exactly one
/// `MessageComplete`.
///
/// `byte_stream` yields `Result<bytes::Bytes, String>`; the `eventsource_stream`
/// decoder reassembles `data:` frames across chunk boundaries so partial frames
/// are handled correctly (ENG-3).
async fn parse_sse_stream<S>(byte_stream: S) -> Result<Vec<Result<ApiStreamEvent, ApiError>>, ApiError>
where
    S: Stream<Item = Result<bytes::Bytes, String>> + Send + Unpin,
{
    use eventsource_stream::Eventsource;

    let mut sse = byte_stream.eventsource();
    let mut accum = AnthropicSseAccum::default();
    let mut events: Vec<Result<ApiStreamEvent, ApiError>> = Vec::new();

    loop {
        match sse.next().await {
            None => break,
            Some(Err(e)) => {
                return Err(ApiError::Network(format!("SSE read error: {e}")));
            }
            Some(Ok(event)) => {
                let data = event.data.trim();
                if data == "[DONE]" {
                    break;
                }
                if data.is_empty() {
                    continue;
                }
                let parsed: serde_json::Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                handle_anthropic_event(&mut accum, &mut events, &parsed);
            }
        }
    }

    // Finalize the accumulated blocks in arrival order.
    let mut content_blocks: Vec<ContentBlock> = Vec::with_capacity(accum.block_order.len());
    for index in &accum.block_order {
        match accum.blocks.remove(index) {
            Some(PendingBlock::Text(text)) => {
                if !text.is_empty() {
                    content_blocks.push(ContentBlock::Text(TextBlock::new(text)));
                }
            }
            Some(PendingBlock::ToolUse {
                id,
                name,
                partial_json,
            }) => {
                let input: std::collections::HashMap<String, serde_json::Value> =
                    serde_json::from_str(&partial_json).unwrap_or_default();
                content_blocks.push(ContentBlock::ToolUse(ToolUseBlock {
                    r#type: "tool_use".into(),
                    id,
                    name,
                    input,
                }));
            }
            None => {}
        }
    }

    events.push(Ok(ApiStreamEvent::MessageComplete(ApiMessageCompleteEvent {
        message: ConversationMessage {
            role: Role::Assistant,
            content: content_blocks,
        },
        usage: accum.usage,
        stop_reason: accum.stop_reason,
    })));

    Ok(events)
}

/// Apply one decoded Anthropic SSE event to the accumulator, pushing any
/// incremental `ApiStreamEvent`s into `events`.
fn handle_anthropic_event(
    accum: &mut AnthropicSseAccum,
    events: &mut Vec<Result<ApiStreamEvent, ApiError>>,
    event: &serde_json::Value,
) {
    match event["type"].as_str().unwrap_or("") {
        "content_block_start" => {
            let index = event["index"].as_u64().unwrap_or(0) as usize;
            let block = &event["content_block"];
            let pending = match block["type"].as_str() {
                Some("tool_use") => PendingBlock::ToolUse {
                    id: block["id"].as_str().unwrap_or("").to_string(),
                    name: block["name"].as_str().unwrap_or("").to_string(),
                    partial_json: String::new(),
                },
                // Default to a text block (covers "text" and any other block we
                // stream as text deltas).
                _ => PendingBlock::Text(String::new()),
            };
            if !accum.blocks.contains_key(&index) {
                accum.block_order.push(index);
            }
            accum.blocks.insert(index, pending);
        }
        "content_block_delta" => {
            let index = event["index"].as_u64().unwrap_or(0) as usize;
            // A delta may arrive before an explicit start (some providers omit
            // content_block_start for the leading text block); open one lazily.
            if !accum.blocks.contains_key(&index) {
                accum.block_order.push(index);
                accum.blocks.insert(index, PendingBlock::Text(String::new()));
            }
            let delta = &event["delta"];
            match delta["type"].as_str() {
                Some("text_delta") => {
                    if let Some(text) = delta["text"].as_str() {
                        if !text.is_empty() {
                            if let Some(PendingBlock::Text(buf)) = accum.blocks.get_mut(&index) {
                                buf.push_str(text);
                            }
                            events.push(Ok(ApiStreamEvent::TextDelta(ApiTextDeltaEvent {
                                text: text.to_string(),
                            })));
                        }
                    }
                }
                Some("input_json_delta") => {
                    if let Some(json_str) = delta["partial_json"].as_str() {
                        if let Some(PendingBlock::ToolUse {
                            id,
                            name,
                            partial_json,
                        }) = accum.blocks.get_mut(&index)
                        {
                            partial_json.push_str(json_str);
                            events.push(Ok(ApiStreamEvent::ToolUseDelta(ApiToolUseDeltaEvent {
                                tool_call_id: id.clone(),
                                name: if name.is_empty() {
                                    None
                                } else {
                                    Some(name.clone())
                                },
                                arguments_delta: json_str.to_string(),
                            })));
                        }
                    }
                }
                _ => {}
            }
        }
        "content_block_stop" => {
            // Block is finalized lazily after the stream ends; nothing to do
            // incrementally (kept for protocol completeness).
        }
        "message_delta" => {
            if let Some(sr) = event["delta"]["stop_reason"].as_str() {
                accum.stop_reason = Some(sr.to_string());
            }
            // message_delta usage is CUMULATIVE and is the source of truth.
            if let Some(u) = event["usage"].as_object() {
                merge_usage(&mut accum.usage, u);
            }
        }
        "message_start" => {
            // Preliminary counts — overwritten by message_delta.
            if let Some(u) = event["message"]["usage"].as_object() {
                merge_usage(&mut accum.usage, u);
            }
        }
        _ => {}
    }
}

/// Overlay any present token-usage fields from a provider `usage` object.
fn merge_usage(usage: &mut UsageSnapshot, u: &serde_json::Map<String, serde_json::Value>) {
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_sse_stream helpers ────────────────────────────────────

    /// Turn an SSE body (frames separated by `\n\n`) into a byte stream and
    /// drive `parse_sse_stream`, returning the full ordered event list.
    async fn run_sse(body: &'static str) -> Vec<Result<ApiStreamEvent, ApiError>> {
        let bytes = bytes::Bytes::from_static(body.as_bytes());
        let stream = futures::stream::iter(vec![Ok::<_, String>(bytes)]);
        parse_sse_stream(stream).await.unwrap()
    }

    /// Drive `parse_sse_stream` and return only the terminal MessageComplete.
    async fn run_sse_complete(body: &'static str) -> ApiMessageCompleteEvent {
        let events = run_sse(body).await;
        match events.into_iter().last() {
            Some(Ok(ApiStreamEvent::MessageComplete(e))) => e,
            other => panic!("expected MessageComplete last, got {:?}", other),
        }
    }

    // ── parse_sse_stream ────────────────────────────────────────────

    #[tokio::test]
    async fn test_parse_sse_response_simple_text() {
        let complete = run_sse_complete(
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello, world!\"}}\n\n\
data: [DONE]\n\n",
        )
        .await;
        let msg = complete.message;
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content.len(), 1);
        match &msg.content[0] {
            ContentBlock::Text(tb) => assert_eq!(tb.text, "Hello, world!"),
            other => panic!("expected Text block, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_parse_sse_response_emits_text_delta_before_complete() {
        let events = run_sse(
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"foo\"}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"bar\"}}\n\n\
data: [DONE]\n\n",
        )
        .await;
        // 2 TextDelta + 1 MessageComplete.
        assert_eq!(events.len(), 3);
        match &events[0] {
            Ok(ApiStreamEvent::TextDelta(e)) => assert_eq!(e.text, "foo"),
            other => panic!("expected TextDelta, got {:?}", other),
        }
        match &events[1] {
            Ok(ApiStreamEvent::TextDelta(e)) => assert_eq!(e.text, "bar"),
            other => panic!("expected TextDelta, got {:?}", other),
        }
        assert!(matches!(&events[2], Ok(ApiStreamEvent::MessageComplete(_))));
    }

    #[tokio::test]
    async fn test_parse_sse_response_tool_use() {
        let complete = run_sse_complete(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read_file\"}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"/tmp/a.txt\\\"}\"}}\n\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
data: [DONE]\n\n",
        )
        .await;
        assert_eq!(complete.message.content.len(), 1);
        match &complete.message.content[0] {
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

    #[tokio::test]
    async fn test_parse_sse_response_tool_use_emits_deltas() {
        // Arguments arrive in two input_json_delta chunks.
        let events = run_sse(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"bash\"}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\":\"}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"ls\\\"}\"}}\n\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
data: [DONE]\n\n",
        )
        .await;
        let tool_deltas: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Ok(ApiStreamEvent::ToolUseDelta(d)) => Some(d),
                _ => None,
            })
            .collect();
        assert_eq!(tool_deltas.len(), 2);
        assert_eq!(tool_deltas[0].tool_call_id, "tu_1");
        assert_eq!(tool_deltas[0].name.as_deref(), Some("bash"));
        // Final assembled tool call parses correctly.
        match events.into_iter().last() {
            Some(Ok(ApiStreamEvent::MessageComplete(c))) => {
                let tu = c.message.tool_uses();
                assert_eq!(tu.len(), 1);
                assert_eq!(tu[0].input.get("cmd").and_then(|v| v.as_str()), Some("ls"));
            }
            other => panic!("expected MessageComplete, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_parse_sse_response_text_then_tool_use() {
        let complete = run_sse_complete(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me check.\"}}\n\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_2\",\"name\":\"bash\"}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}\n\n\
data: {\"type\":\"content_block_stop\",\"index\":1}\n\n\
data: [DONE]\n\n",
        )
        .await;
        assert_eq!(complete.message.content.len(), 2);
        match &complete.message.content[0] {
            ContentBlock::Text(tb) => assert_eq!(tb.text, "Let me check."),
            other => panic!("expected Text block, got {:?}", other),
        }
        match &complete.message.content[1] {
            ContentBlock::ToolUse(tu) => assert_eq!(tu.name, "bash"),
            other => panic!("expected ToolUse block, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_parse_sse_response_message_start_extracts_input_tokens() {
        let complete = run_sse_complete(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":42}}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n\
data: [DONE]\n\n",
        )
        .await;
        assert_eq!(complete.usage.input_tokens, 42);
    }

    #[tokio::test]
    async fn test_parse_sse_response_message_delta_extracts_output_tokens_and_stop_reason() {
        let complete = run_sse_complete(
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":17}}\n\n\
data: [DONE]\n\n",
        )
        .await;
        assert_eq!(complete.usage.output_tokens, 17);
        assert_eq!(complete.stop_reason.as_deref(), Some("end_turn"));
    }

    #[tokio::test]
    async fn test_parse_sse_response_empty_body() {
        let stream = futures::stream::iter(Vec::<Result<bytes::Bytes, String>>::new());
        let events = parse_sse_stream(stream).await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            Ok(ApiStreamEvent::MessageComplete(c)) => {
                assert_eq!(c.message.role, Role::Assistant);
                assert!(c.message.content.is_empty());
                assert_eq!(c.usage.input_tokens, 0);
                assert_eq!(c.usage.output_tokens, 0);
                assert!(c.stop_reason.is_none());
            }
            other => panic!("expected MessageComplete, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_parse_sse_response_ignores_non_data_lines() {
        let complete = run_sse_complete(
            "event: ping\ndata: {\"type\":\"ping\"}\n\n\
: comment\n\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n\
data: [DONE]\n\n",
        )
        .await;
        assert_eq!(complete.message.content.len(), 1);
    }

    #[tokio::test]
    async fn test_parse_sse_response_split_across_chunks() {
        // A single `data:` frame split mid-JSON across two byte chunks; the
        // eventsource decoder must reassemble it.
        let chunks: Vec<Result<bytes::Bytes, String>> = vec![
            Ok(bytes::Bytes::from_static(
                b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_",
            )),
            Ok(bytes::Bytes::from_static(
                b"delta\",\"text\":\"split\"}}\n\ndata: [DONE]\n\n",
            )),
        ];
        let stream = futures::stream::iter(chunks);
        let events = parse_sse_stream(stream).await.unwrap();
        match events.into_iter().last() {
            Some(Ok(ApiStreamEvent::MessageComplete(c))) => {
                assert_eq!(c.message.text(), "split");
            }
            other => panic!("expected MessageComplete, got {:?}", other),
        }
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
    fn test_is_retryable_http_retryable_status() {
        assert!(is_retryable(&ApiError::Http {
            status: 529,
            body: "overloaded".into(),
        }));
    }

    #[test]
    fn test_is_retryable_http_non_retryable_status() {
        assert!(!is_retryable(&ApiError::Http {
            status: 400,
            body: "bad request".into(),
        }));
    }

    /// Regression for ENG-6: a 400 whose body *mentions* a retryable code must
    /// NOT be retried — the decision is on the integer status, never the body.
    #[test]
    fn test_is_retryable_http_400_body_mentions_503() {
        assert!(!is_retryable(&ApiError::Http {
            status: 400,
            body: "upstream returned 503 earlier but this is a 400".into(),
        }));
    }

    #[test]
    fn test_map_http_error_maps_status() {
        assert!(matches!(
            map_http_error(401, "x".into()),
            ApiError::Authentication(_)
        ));
        assert!(matches!(
            map_http_error(403, "x".into()),
            ApiError::Authentication(_)
        ));
        assert!(matches!(
            map_http_error(429, "x".into()),
            ApiError::RateLimit(_)
        ));
        match map_http_error(503, "overloaded".into()) {
            ApiError::Http { status, body } => {
                assert_eq!(status, 503);
                assert_eq!(body, "overloaded");
            }
            other => panic!("expected Http, got {:?}", other),
        }
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
        assert!((1.0..=1.25).contains(&d0), "d0 = {d0}");
        assert!((2.0..=2.5).contains(&d1), "d1 = {d1}");
        assert!((4.0..=5.0).contains(&d2), "d2 = {d2}");
        assert!(d1 > d0);
        assert!(d2 > d1);
    }

    #[test]
    fn test_get_retry_delay_capped_at_max() {
        // attempt 10 → 2^10 = 1024, capped at MAX_DELAY_SECS (30)
        let d = get_retry_delay(10);
        assert!(
            (MAX_DELAY_SECS..=MAX_DELAY_SECS * 1.25).contains(&d),
            "d = {d}"
        );
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
