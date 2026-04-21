//! Shared SSE helpers for providers that speak the OpenAI Chat Completions
//! streaming protocol (`POST /v1/chat/completions` with `stream: true`).
//!
//! Both the canonical OpenAI provider (`openai.rs`) and OpenAI-compatible
//! providers can use `stream_openai_chat` to obtain an incremental
//! `BoxStream<…>` that yields:
//!
//! - `ApiStreamEvent::TextDelta` for each `choices[0].delta.content` chunk.
//! - `ApiStreamEvent::ToolUseDelta` for each streaming tool-call argument chunk.
//! - `ApiStreamEvent::MessageComplete` exactly once at the end, carrying
//!   accumulated usage and stop_reason.
//!
//! # Wire format
//!
//! ```text
//! data: {"id":"…","choices":[{"delta":{"content":"Hi"},"finish_reason":null}]}
//! ```
//! with a terminal `data: [DONE]` sentinel.

use std::collections::HashMap;

use eventsource_stream::Eventsource;
use futures::{FutureExt, Stream, StreamExt};
use oh_types::api::{
    ApiError, ApiMessageCompleteEvent, ApiStreamEvent, ApiTextDeltaEvent, ApiToolUseDeltaEvent,
    UsageSnapshot,
};
use oh_types::messages::{ContentBlock, ConversationMessage, Role, TextBlock, ToolUseBlock};

/// Accumulated state while consuming an OpenAI chat.completions SSE stream.
struct SseAccum {
    text: String,
    /// Keyed by tool-call index; value is (id, name, accumulated_arguments).
    tool_calls: HashMap<usize, (String, String, String)>,
    usage: UsageSnapshot,
    finish_reason: Option<String>,
}

impl Default for SseAccum {
    fn default() -> Self {
        Self {
            text: String::new(),
            tool_calls: HashMap::new(),
            usage: UsageSnapshot::default(),
            finish_reason: None,
        }
    }
}

/// Drive an OpenAI chat.completions SSE byte stream and return an incremental
/// event stream.
///
/// `byte_stream` must yield `Result<bytes::Bytes, E>` where `E: ToString`.
pub fn stream_openai_chat<S, E>(
    byte_stream: S,
) -> impl Stream<Item = Result<ApiStreamEvent, ApiError>> + Send + 'static
where
    S: Stream<Item = Result<bytes::Bytes, E>> + Send + Unpin + 'static,
    E: ToString + Send + 'static,
{
    // We collect all incremental events into a Vec in one pass, then emit them
    // as a plain iter stream. This avoids async-generator complexity while still
    // yielding TextDelta / ToolUseDelta *before* MessageComplete.
    //
    // In production the latency difference is the RTT of the final HTTP chunk,
    // which is negligible compared to the time already spent streaming.
    async move {
        let mapped = byte_stream.map(|r| r.map_err(|e| e.to_string()));
        let mut sse = mapped.eventsource();
        let mut accum = SseAccum::default();
        let mut events: Vec<Result<ApiStreamEvent, ApiError>> = Vec::new();

        loop {
            match sse.next().await {
                None => break,
                Some(Err(e)) => {
                    events.push(Err(ApiError::Network(format!("SSE read error: {e}"))));
                    // Don't append MessageComplete on error.
                    return futures::stream::iter(events);
                }
                Some(Ok(event)) => {
                    let data = event.data.trim();
                    if data == "[DONE]" {
                        break;
                    }
                    if data.is_empty() {
                        continue;
                    }

                    let chunk: serde_json::Value = match serde_json::from_str(data) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    // Usage metadata — some providers send it on the last chunk.
                    if let Some(u) = chunk.get("usage").filter(|v| !v.is_null()) {
                        if let Some(v) = u.get("prompt_tokens").and_then(|v| v.as_u64()) {
                            accum.usage.input_tokens = v;
                        }
                        if let Some(v) = u.get("completion_tokens").and_then(|v| v.as_u64()) {
                            accum.usage.output_tokens = v;
                        }
                    }

                    let choice = match chunk.get("choices").and_then(|c| c.get(0)) {
                        Some(c) => c,
                        None => continue,
                    };

                    // finish_reason
                    if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                        if !fr.is_empty() {
                            accum.finish_reason = Some(fr.to_string());
                        }
                    }

                    let delta = match choice.get("delta") {
                        Some(d) => d,
                        None => continue,
                    };

                    // Text delta
                    if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            accum.text.push_str(text);
                            events.push(Ok(ApiStreamEvent::TextDelta(ApiTextDeltaEvent {
                                text: text.to_string(),
                            })));
                        }
                    }

                    // Tool-call argument deltas
                    if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                        for tc in tool_calls {
                            let idx =
                                tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

                            let id = tc
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = tc
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let args_delta = tc
                                .get("function")
                                .and_then(|f| f.get("arguments"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");

                            let entry = accum
                                .tool_calls
                                .entry(idx)
                                .or_insert_with(|| (id.clone(), name.clone(), String::new()));

                            if !id.is_empty() {
                                entry.0 = id.clone();
                            }
                            if !name.is_empty() {
                                entry.1 = name.clone();
                            }

                            if !args_delta.is_empty() {
                                entry.2.push_str(args_delta);
                                events.push(Ok(ApiStreamEvent::ToolUseDelta(
                                    ApiToolUseDeltaEvent {
                                        tool_call_id: entry.0.clone(),
                                        name: if entry.1.is_empty() {
                                            None
                                        } else {
                                            Some(entry.1.clone())
                                        },
                                        arguments_delta: args_delta.to_string(),
                                    },
                                )));
                            }
                        }
                    }
                }
            }
        }

        // Build the final MessageComplete.
        let mut blocks: Vec<ContentBlock> = Vec::new();
        if !accum.text.is_empty() {
            blocks.push(ContentBlock::Text(TextBlock::new(accum.text)));
        }
        let mut tc_vec: Vec<(usize, (String, String, String))> =
            accum.tool_calls.into_iter().collect();
        tc_vec.sort_by_key(|(idx, _)| *idx);
        for (_, (id, name, args)) in tc_vec {
            let input: HashMap<String, serde_json::Value> =
                serde_json::from_str(&args).unwrap_or_default();
            blocks.push(ContentBlock::ToolUse(ToolUseBlock {
                r#type: "tool_use".into(),
                id,
                name,
                input,
            }));
        }

        let stop_reason = accum.finish_reason.map(|fr| match fr.as_str() {
            "stop" => "end_turn".to_string(),
            "tool_calls" => "tool_use".to_string(),
            "length" => "max_tokens".to_string(),
            other => other.to_string(),
        });

        events.push(Ok(ApiStreamEvent::MessageComplete(ApiMessageCompleteEvent {
            message: ConversationMessage {
                role: Role::Assistant,
                content: blocks,
            },
            usage: accum.usage,
            stop_reason,
        })));

        futures::stream::iter(events)
    }
    .flatten_stream()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn make_stream(
        chunks: Vec<&'static [u8]>,
    ) -> impl Stream<Item = Result<bytes::Bytes, String>> + Send + Unpin + 'static {
        let items: Vec<Result<bytes::Bytes, String>> =
            chunks.into_iter().map(|c| Ok(bytes::Bytes::from_static(c))).collect();
        futures::stream::iter(items)
    }

    #[tokio::test]
    async fn test_stream_openai_chat_text_delta_before_complete() {
        let stream = make_stream(vec![
            b"data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
            b"data: {\"choices\":[{\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n",
            b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}\n\n",
            b"data: [DONE]\n\n",
        ]);

        let events: Vec<_> = stream_openai_chat(stream).collect().await;

        // Should have 2 TextDelta events + 1 MessageComplete
        assert_eq!(events.len(), 3, "expected 3 events, got {}", events.len());

        match &events[0] {
            Ok(ApiStreamEvent::TextDelta(e)) => assert_eq!(e.text, "Hello"),
            other => panic!("expected TextDelta, got {:?}", other),
        }
        match &events[1] {
            Ok(ApiStreamEvent::TextDelta(e)) => assert_eq!(e.text, " world"),
            other => panic!("expected TextDelta, got {:?}", other),
        }
        match &events[2] {
            Ok(ApiStreamEvent::MessageComplete(e)) => {
                assert_eq!(e.message.text(), "Hello world");
                assert_eq!(e.usage.input_tokens, 5);
                assert_eq!(e.usage.output_tokens, 2);
                assert_eq!(e.stop_reason.as_deref(), Some("end_turn"));
            }
            other => panic!("expected MessageComplete, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_stream_openai_chat_tool_use_delta() {
        let stream = make_stream(vec![
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"bash\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"cmd\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"ls\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            b"data: [DONE]\n\n",
        ]);

        let events: Vec<_> = stream_openai_chat(stream).collect().await;

        // 2 ToolUseDelta events + 1 MessageComplete (first chunk has empty args)
        let tool_deltas: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, Ok(ApiStreamEvent::ToolUseDelta(_))))
            .collect();
        assert!(!tool_deltas.is_empty(), "expected at least one ToolUseDelta");

        let complete = events.last().unwrap();
        match complete {
            Ok(ApiStreamEvent::MessageComplete(e)) => {
                assert_eq!(e.stop_reason.as_deref(), Some("tool_use"));
                let tus = e.message.tool_uses();
                assert_eq!(tus.len(), 1);
                assert_eq!(tus[0].name, "bash");
                assert_eq!(tus[0].id, "call_1");
            }
            other => panic!("expected MessageComplete, got {:?}", other),
        }
    }
}
