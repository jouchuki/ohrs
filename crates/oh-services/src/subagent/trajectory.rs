//! `TrajectoryRecorder` — records an agent run's messages as a parent-linked
//! session trajectory.
//!
//! Recording is implemented as a hook ([`HookExecutorTrait`]) so it rides the
//! same mechanism as webhooks and blocks: register it (or fan it in) as a
//! `QueryContext::hook_executor` and it appends a [`SessionMessage`] to its
//! [`SessionStore`] for the relevant lifecycle events.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use oh_hooks::{AggregatedHookResult, HookEvent, HookExecutorTrait};
use uuid::Uuid;

use crate::sessions::{MessageRole, SessionMessage, SessionStore};

/// Appends agent-run messages to a [`SessionStore`] under a fixed session id.
///
/// The recorder is intentionally permissive: it never blocks (always returns an
/// empty [`AggregatedHookResult`]) and swallows store errors after logging, so
/// recording can never abort the agent run.
pub struct TrajectoryRecorder {
    store: Arc<SessionStore>,
    session_id: String,
    seq: AtomicU64,
}

impl TrajectoryRecorder {
    /// Create a recorder writing into `session_id` on `store`.
    pub fn new(store: Arc<SessionStore>, session_id: impl Into<String>) -> Self {
        Self {
            store,
            session_id: session_id.into(),
            seq: AtomicU64::new(0),
        }
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::SeqCst)
    }

    /// Map a lifecycle event to the role under which its payload is recorded,
    /// or `None` for events that are not part of the trajectory.
    fn role_for(event: HookEvent) -> Option<MessageRole> {
        match event {
            HookEvent::PostUserMessage | HookEvent::SubagentStart => Some(MessageRole::User),
            HookEvent::PostApiResponse | HookEvent::SubagentStop => Some(MessageRole::Assistant),
            HookEvent::PostToolUse | HookEvent::SubagentMessage => Some(MessageRole::Tool),
            _ => None,
        }
    }
}

#[async_trait]
impl HookExecutorTrait for TrajectoryRecorder {
    async fn execute(&self, event: HookEvent, payload: serde_json::Value) -> AggregatedHookResult {
        if let Some(role) = Self::role_for(event) {
            let msg = SessionMessage {
                id: Uuid::new_v4().to_string(),
                session_id: self.session_id.clone(),
                role,
                content: serde_json::json!({ "event": event.to_string(), "payload": payload }),
                seq: self.next_seq(),
                created_at: SystemTime::now(),
            };
            if let Err(e) = self.store.append_message(&msg).await {
                tracing::warn!("trajectory recorder: append failed: {e}");
            }
        }
        AggregatedHookResult::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{SessionRecord, SessionStatus, SqliteBackend};
    use std::path::Path;

    async fn make_store(dir: &Path) -> Arc<SessionStore> {
        let backend = SqliteBackend::new(&dir.join("traj.db")).await.unwrap();
        Arc::new(SessionStore::with_backend(Box::new(backend)))
    }

    fn session(id: &str, parent: Option<&str>, root: &Path) -> SessionRecord {
        SessionRecord {
            id: id.to_string(),
            name: None,
            project_root: root.to_path_buf(),
            model: "test".into(),
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            message_count: 0,
            status: SessionStatus::Active,
            parent_session_id: parent.map(String::from),
        }
    }

    #[tokio::test]
    async fn test_recorder_appends_messages_for_relevant_events() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path()).await;
        store
            .create_session(&session("sess-traj", None, dir.path()))
            .await
            .unwrap();

        let recorder = TrajectoryRecorder::new(Arc::clone(&store), "sess-traj");

        // Relevant events are recorded; irrelevant ones are ignored.
        recorder
            .execute(
                HookEvent::SubagentStart,
                serde_json::json!({"prompt": "hi"}),
            )
            .await;
        recorder
            .execute(HookEvent::PreApiRequest, serde_json::json!({}))
            .await; // ignored
        recorder
            .execute(
                HookEvent::SubagentStop,
                serde_json::json!({"result": "done"}),
            )
            .await;

        let msgs = store.list_messages("sess-traj", None).await.unwrap();
        assert_eq!(msgs.len(), 2, "only relevant events recorded: {msgs:?}");
        assert_eq!(msgs[0].role, MessageRole::User);
        assert_eq!(msgs[1].role, MessageRole::Assistant);
        // seq is monotonic.
        assert_eq!(msgs[0].seq, 0);
        assert_eq!(msgs[1].seq, 1);
    }

    #[tokio::test]
    async fn test_recorder_links_parent_and_child_trajectories() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path()).await;

        store
            .create_session(&session("parent", None, dir.path()))
            .await
            .unwrap();
        store
            .create_session(&session("child", Some("parent"), dir.path()))
            .await
            .unwrap();

        let parent_rec = TrajectoryRecorder::new(Arc::clone(&store), "parent");
        let child_rec = TrajectoryRecorder::new(Arc::clone(&store), "child");

        parent_rec
            .execute(HookEvent::SubagentStart, serde_json::json!({}))
            .await;
        child_rec
            .execute(HookEvent::SubagentStart, serde_json::json!({}))
            .await;
        child_rec
            .execute(HookEvent::SubagentStop, serde_json::json!({}))
            .await;

        // Child session links back to parent and has its own messages.
        let child = store.get_session("child").await.unwrap().unwrap();
        assert_eq!(child.parent_session_id, Some("parent".to_string()));

        let parent_msgs = store.list_messages("parent", None).await.unwrap();
        let child_msgs = store.list_messages("child", None).await.unwrap();
        assert_eq!(parent_msgs.len(), 1);
        assert_eq!(child_msgs.len(), 2);
    }

    /// Round-trip a FULL transcript through a real `SqliteBackend`: a parent
    /// session and a child session linked via `parent_session_id`, each with a
    /// User (prompt text), an Assistant (text) and a Tool (I/O) row. Asserts the
    /// actual content survives, not just the row counts.
    #[tokio::test]
    async fn test_full_transcript_round_trips_with_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path()).await;

        store
            .create_session(&session("parent", None, dir.path()))
            .await
            .unwrap();
        store
            .create_session(&session("child", Some("parent"), dir.path()))
            .await
            .unwrap();

        let parent_rec = TrajectoryRecorder::new(Arc::clone(&store), "parent");
        let child_rec = TrajectoryRecorder::new(Arc::clone(&store), "child");

        // Parent agent transcript: user prompt → assistant text → tool I/O.
        parent_rec
            .execute(
                HookEvent::PostUserMessage,
                serde_json::json!({"text": "parent prompt: spawn a subagent"}),
            )
            .await;
        parent_rec
            .execute(
                HookEvent::PostApiResponse,
                serde_json::json!({"text": "I will spawn one", "tool_uses": []}),
            )
            .await;
        parent_rec
            .execute(
                HookEvent::PostToolUse,
                serde_json::json!({
                    "tool_name": "Agent",
                    "tool_input": {"prompt": "reply deep-pong"},
                    "tool_output": "deep-pong",
                }),
            )
            .await;

        // Child agent transcript.
        child_rec
            .execute(
                HookEvent::PostUserMessage,
                serde_json::json!({"text": "reply deep-pong"}),
            )
            .await;
        child_rec
            .execute(
                HookEvent::PostApiResponse,
                serde_json::json!({"text": "deep-pong", "tool_uses": []}),
            )
            .await;

        // Parent links nowhere; child links to parent.
        let child = store.get_session("child").await.unwrap().unwrap();
        assert_eq!(child.parent_session_id.as_deref(), Some("parent"));
        let parent = store.get_session("parent").await.unwrap().unwrap();
        assert_eq!(parent.parent_session_id, None);

        let parent_msgs = store.list_messages("parent", None).await.unwrap();
        let child_msgs = store.list_messages("child", None).await.unwrap();
        assert_eq!(parent_msgs.len(), 3);
        assert_eq!(child_msgs.len(), 2);

        // Roles map as expected.
        assert_eq!(parent_msgs[0].role, MessageRole::User);
        assert_eq!(parent_msgs[1].role, MessageRole::Assistant);
        assert_eq!(parent_msgs[2].role, MessageRole::Tool);

        // Content survives the round-trip (assert the actual text, not counts).
        assert_eq!(
            parent_msgs[0].content["payload"]["text"],
            "parent prompt: spawn a subagent"
        );
        assert_eq!(
            parent_msgs[1].content["payload"]["text"],
            "I will spawn one"
        );
        assert_eq!(parent_msgs[2].content["payload"]["tool_name"], "Agent");
        assert_eq!(
            parent_msgs[2].content["payload"]["tool_output"],
            "deep-pong"
        );

        assert_eq!(child_msgs[0].content["payload"]["text"], "reply deep-pong");
        assert_eq!(child_msgs[1].content["payload"]["text"], "deep-pong");
        assert_eq!(child_msgs[1].role, MessageRole::Assistant);
    }
}
