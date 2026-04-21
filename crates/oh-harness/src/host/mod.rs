//! JSON-lines IPC host mode.
//!
//! Reads `FrontendRequest` from stdin (lines prefixed with `OHJSON:`) and
//! writes `BackendEvent` to stdout with the same prefix.  Non-`OHJSON:`
//! lines on stdin are silently ignored.

use std::sync::Arc;

use oh_engine::QueryEngine;
use oh_types::stream_events::StreamEvent;
use oh_types::ui::{BackendEvent, BackendEventType, FrontendRequest, TranscriptItem};
use serde_json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::sync::Mutex;

const PREFIX: &str = "OHJSON:";

/// Serialize a `BackendEvent` and write it to the writer channel.
fn event_line(event: &BackendEvent) -> String {
    let json = serde_json::to_string(event).unwrap_or_else(|e| {
        serde_json::to_string(&BackendEvent {
            r#type: BackendEventType::Error,
            message: Some(format!("serialization error: {e}")),
            ..Default::default()
        })
        .unwrap()
    });
    format!("{PREFIX}{json}\n")
}

/// Run the JSON-lines host loop.
///
/// Reads lines from stdin; any line starting with `OHJSON:` is decoded as a
/// `FrontendRequest` and dispatched.  All other lines are ignored.
/// `BackendEvent`s are written to stdout prefixed with `OHJSON:`.
pub async fn run_host(engine: QueryEngine) -> Result<(), Box<dyn std::error::Error>> {
    // Wrap engine in an Arc<Mutex> so we can share it across the async tasks.
    let engine = Arc::new(Mutex::new(engine));

    // Writer channel — all stdout writes go through here so they are
    // serialised and we never interleave partial lines.
    let (tx, mut rx) = mpsc::channel::<String>(256);

    // Spawn the writer task.
    let writer_handle = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = rx.recv().await {
            let _ = stdout.write_all(line.as_bytes()).await;
            let _ = stdout.flush().await;
        }
    });

    // Read stdin line by line.
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    while let Ok(Some(line)) = reader.next_line().await {
        let trimmed = line.trim();

        // Ignore non-protocol lines.
        if !trimmed.starts_with(PREFIX) {
            continue;
        }

        let payload = &trimmed[PREFIX.len()..];

        let request: FrontendRequest = match serde_json::from_str(payload) {
            Ok(r) => r,
            Err(e) => {
                let err_event = BackendEvent {
                    r#type: BackendEventType::Error,
                    message: Some(format!("parse error: {e}")),
                    ..Default::default()
                };
                let _ = tx.send(event_line(&err_event)).await;
                continue;
            }
        };

        match request {
            FrontendRequest::Shutdown => {
                let shutdown_event = BackendEvent {
                    r#type: BackendEventType::Shutdown,
                    ..Default::default()
                };
                let _ = tx.send(event_line(&shutdown_event)).await;
                break;
            }

            FrontendRequest::SubmitLine { line: text } => {
                let text = text.unwrap_or_default();
                if text.is_empty() {
                    continue;
                }

                let mut eng = engine.lock().await;
                match eng.submit_message(&text).await {
                    Ok(events) => {
                        for (ev, _usage) in &events {
                            if let Some(be) = stream_event_to_backend(ev) {
                                let _ = tx.send(event_line(&be)).await;
                            }
                        }
                        // Signal that this turn is complete.
                        let done = BackendEvent {
                            r#type: BackendEventType::LineComplete,
                            ..Default::default()
                        };
                        let _ = tx.send(event_line(&done)).await;
                    }
                    Err(e) => {
                        let err_event = BackendEvent {
                            r#type: BackendEventType::Error,
                            message: Some(format!("engine error: {e}")),
                            ..Default::default()
                        };
                        let _ = tx.send(event_line(&err_event)).await;
                    }
                }
            }

            // Permission / question responses are handled in-band via the
            // engine's callback closures; here we simply acknowledge.
            FrontendRequest::PermissionResponse { .. } | FrontendRequest::QuestionResponse { .. } => {
                // No-op for now — permission handling is synchronous inside the
                // FullAuto permission checker.  Future work: wire a oneshot channel.
            }

            FrontendRequest::ListSessions => {
                // Not yet implemented — return an empty list so the frontend
                // doesn't hang.
                let ev = BackendEvent {
                    r#type: BackendEventType::StateSnapshot,
                    message: Some("[]".into()),
                    ..Default::default()
                };
                let _ = tx.send(event_line(&ev)).await;
            }
        }
    }

    // Drop the sender so the writer task drains and exits.
    drop(tx);
    let _ = writer_handle.await;

    Ok(())
}

/// Translate a `StreamEvent` from the engine into a `BackendEvent` for the
/// frontend.  Returns `None` for event types that don't need to be forwarded.
fn stream_event_to_backend(ev: &StreamEvent) -> Option<BackendEvent> {
    match ev {
        StreamEvent::AssistantTextDelta(delta) => Some(BackendEvent {
            r#type: BackendEventType::AssistantDelta,
            message: Some(delta.text.clone()),
            ..Default::default()
        }),

        StreamEvent::AssistantTurnComplete(turn) => {
            let text = turn.message.text();
            let item = TranscriptItem {
                role: "assistant".into(),
                text: text.clone(),
                tool_name: None,
                tool_input: None,
                is_error: Some(false),
            };
            Some(BackendEvent {
                r#type: BackendEventType::AssistantComplete,
                message: Some(text),
                item: Some(item),
                ..Default::default()
            })
        }

        StreamEvent::ToolExecutionStarted(started) => Some(BackendEvent {
            r#type: BackendEventType::ToolStarted,
            tool_name: Some(started.tool_name.clone()),
            tool_input: serde_json::to_value(&started.tool_input).ok(),
            ..Default::default()
        }),

        StreamEvent::ToolExecutionCompleted(completed) => Some(BackendEvent {
            r#type: BackendEventType::ToolCompleted,
            tool_name: Some(completed.tool_name.clone()),
            output: Some(completed.output.clone()),
            is_error: Some(completed.is_error),
            ..Default::default()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oh_types::ui::{BackendEventType, FrontendRequest};

    // ── Serde round-trip tests ─────────────────────────────────────────────

    #[test]
    fn test_frontend_request_submit_line_roundtrip() {
        let req = FrontendRequest::SubmitLine {
            line: Some("hello world".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: FrontendRequest = serde_json::from_str(&json).unwrap();
        match back {
            FrontendRequest::SubmitLine { line } => assert_eq!(line, Some("hello world".into())),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_frontend_request_submit_line_null_roundtrip() {
        let req = FrontendRequest::SubmitLine { line: None };
        let json = serde_json::to_string(&req).unwrap();
        let back: FrontendRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, FrontendRequest::SubmitLine { line: None }));
    }

    #[test]
    fn test_frontend_request_permission_response_roundtrip() {
        let req = FrontendRequest::PermissionResponse {
            request_id: Some("req-1".into()),
            allowed: Some(true),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: FrontendRequest = serde_json::from_str(&json).unwrap();
        match back {
            FrontendRequest::PermissionResponse { request_id, allowed } => {
                assert_eq!(request_id, Some("req-1".into()));
                assert_eq!(allowed, Some(true));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_frontend_request_question_response_roundtrip() {
        let req = FrontendRequest::QuestionResponse {
            request_id: Some("q-42".into()),
            answer: Some("yes please".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: FrontendRequest = serde_json::from_str(&json).unwrap();
        match back {
            FrontendRequest::QuestionResponse { request_id, answer } => {
                assert_eq!(request_id, Some("q-42".into()));
                assert_eq!(answer, Some("yes please".into()));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_frontend_request_list_sessions_roundtrip() {
        let req = FrontendRequest::ListSessions;
        let json = serde_json::to_string(&req).unwrap();
        let back: FrontendRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, FrontendRequest::ListSessions));
    }

    #[test]
    fn test_frontend_request_shutdown_roundtrip() {
        let req = FrontendRequest::Shutdown;
        let json = serde_json::to_string(&req).unwrap();
        let back: FrontendRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, FrontendRequest::Shutdown));
    }

    #[test]
    fn test_backend_event_ready_roundtrip() {
        let ev = BackendEvent {
            r#type: BackendEventType::Ready,
            message: Some("ready".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: BackendEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.r#type, BackendEventType::Ready));
        assert_eq!(back.message, Some("ready".into()));
    }

    #[test]
    fn test_backend_event_assistant_delta_roundtrip() {
        let ev = BackendEvent {
            r#type: BackendEventType::AssistantDelta,
            message: Some("Hello, ".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: BackendEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.r#type, BackendEventType::AssistantDelta));
        assert_eq!(back.message, Some("Hello, ".into()));
    }

    #[test]
    fn test_backend_event_tool_started_roundtrip() {
        let ev = BackendEvent {
            r#type: BackendEventType::ToolStarted,
            tool_name: Some("bash".into()),
            tool_input: Some(serde_json::json!({"command": "ls"})),
            ..Default::default()
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: BackendEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.r#type, BackendEventType::ToolStarted));
        assert_eq!(back.tool_name, Some("bash".into()));
    }

    #[test]
    fn test_backend_event_tool_completed_roundtrip() {
        let ev = BackendEvent {
            r#type: BackendEventType::ToolCompleted,
            tool_name: Some("bash".into()),
            output: Some("file1\nfile2\n".into()),
            is_error: Some(false),
            ..Default::default()
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: BackendEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.r#type, BackendEventType::ToolCompleted));
        assert_eq!(back.output, Some("file1\nfile2\n".into()));
        assert_eq!(back.is_error, Some(false));
    }

    #[test]
    fn test_backend_event_error_roundtrip() {
        let ev = BackendEvent {
            r#type: BackendEventType::Error,
            message: Some("something went wrong".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: BackendEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.r#type, BackendEventType::Error));
    }

    #[test]
    fn test_backend_event_shutdown_roundtrip() {
        let ev = BackendEvent {
            r#type: BackendEventType::Shutdown,
            ..Default::default()
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: BackendEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.r#type, BackendEventType::Shutdown));
    }

    // ── Protocol line format tests ─────────────────────────────────────────

    #[test]
    fn test_event_line_has_prefix() {
        let ev = BackendEvent {
            r#type: BackendEventType::Ready,
            ..Default::default()
        };
        let line = event_line(&ev);
        assert!(line.starts_with(PREFIX));
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn test_event_line_is_valid_json_after_prefix() {
        let ev = BackendEvent {
            r#type: BackendEventType::AssistantDelta,
            message: Some("hi".into()),
            ..Default::default()
        };
        let line = event_line(&ev);
        let json_part = line.trim_start_matches(PREFIX).trim_end();
        let v: serde_json::Value = serde_json::from_str(json_part).expect("valid JSON");
        assert_eq!(v["type"], "assistant_delta");
        assert_eq!(v["message"], "hi");
    }

    // ── Non-OHJSON line ignored (logic test) ──────────────────────────────

    #[test]
    fn test_non_ohjson_line_is_ignored() {
        // Simulate the check done in run_host.
        let lines = [
            "just a normal line",
            "another line",
            "  whitespace but no prefix",
            "OHJSON_FAKE:{\"type\":\"shutdown\"}",  // close but wrong
        ];
        for line in &lines {
            assert!(
                !line.trim().starts_with(PREFIX),
                "line should NOT start with OHJSON: — got: {line}"
            );
        }
    }

    #[test]
    fn test_ohjson_line_is_recognized() {
        let line = r#"OHJSON:{"type":"shutdown"}"#;
        assert!(line.trim().starts_with(PREFIX));
        let payload = &line[PREFIX.len()..];
        let req: FrontendRequest = serde_json::from_str(payload).unwrap();
        assert!(matches!(req, FrontendRequest::Shutdown));
    }

    // ── Complete BackendEventType serde coverage ───────────────────────────

    #[test]
    fn test_backend_event_state_snapshot_roundtrip() {
        let ev = BackendEvent {
            r#type: BackendEventType::StateSnapshot,
            state: Some(serde_json::json!({"model": "claude-3"})),
            ..Default::default()
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: BackendEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.r#type, BackendEventType::StateSnapshot));
        assert!(back.state.is_some());
    }

    #[test]
    fn test_backend_event_tasks_snapshot_roundtrip() {
        let ev = BackendEvent {
            r#type: BackendEventType::TasksSnapshot,
            tasks: Some(vec![]),
            ..Default::default()
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: BackendEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.r#type, BackendEventType::TasksSnapshot));
    }

    #[test]
    fn test_backend_event_transcript_item_roundtrip() {
        let item = TranscriptItem {
            role: "user".into(),
            text: "hello".into(),
            tool_name: None,
            tool_input: None,
            is_error: Some(false),
        };
        let ev = BackendEvent {
            r#type: BackendEventType::TranscriptItem,
            item: Some(item),
            ..Default::default()
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: BackendEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.r#type, BackendEventType::TranscriptItem));
        let it = back.item.unwrap();
        assert_eq!(it.role, "user");
        assert_eq!(it.text, "hello");
    }

    #[test]
    fn test_backend_event_assistant_complete_roundtrip() {
        let ev = BackendEvent {
            r#type: BackendEventType::AssistantComplete,
            message: Some("Done.".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: BackendEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.r#type, BackendEventType::AssistantComplete));
        assert_eq!(back.message, Some("Done.".into()));
    }

    #[test]
    fn test_backend_event_line_complete_roundtrip() {
        let ev = BackendEvent {
            r#type: BackendEventType::LineComplete,
            ..Default::default()
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: BackendEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.r#type, BackendEventType::LineComplete));
    }

    #[test]
    fn test_backend_event_clear_transcript_roundtrip() {
        let ev = BackendEvent {
            r#type: BackendEventType::ClearTranscript,
            ..Default::default()
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: BackendEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.r#type, BackendEventType::ClearTranscript));
    }

    #[test]
    fn test_backend_event_modal_request_roundtrip() {
        let ev = BackendEvent {
            r#type: BackendEventType::ModalRequest,
            modal: Some(serde_json::json!({"kind": "permission", "tool": "bash"})),
            ..Default::default()
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: BackendEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.r#type, BackendEventType::ModalRequest));
        assert!(back.modal.is_some());
    }

    #[test]
    fn test_backend_event_select_request_roundtrip() {
        let ev = BackendEvent {
            r#type: BackendEventType::SelectRequest,
            select_options: Some(vec![serde_json::json!("option1"), serde_json::json!("option2")]),
            ..Default::default()
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: BackendEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.r#type, BackendEventType::SelectRequest));
        assert_eq!(back.select_options.unwrap().len(), 2);
    }
}
