//! UI protocol types for the React TUI backend.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::state::AppState;
use crate::tasks::TaskRecord;

/// One request sent from the React frontend to the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FrontendRequest {
    #[serde(rename = "submit_line")]
    SubmitLine { line: Option<String> },
    #[serde(rename = "permission_response")]
    PermissionResponse {
        request_id: Option<String>,
        allowed: Option<bool>,
    },
    #[serde(rename = "question_response")]
    QuestionResponse {
        request_id: Option<String>,
        answer: Option<String>,
    },
    #[serde(rename = "list_sessions")]
    ListSessions,
    #[serde(rename = "shutdown")]
    Shutdown,
}

/// One transcript row rendered by the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptItem {
    pub role: String,
    pub text: String,
    pub tool_name: Option<String>,
    pub tool_input: Option<HashMap<String, serde_json::Value>>,
    pub is_error: Option<bool>,
}

/// UI-safe task representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub id: String,
    pub task_type: String,
    pub status: String,
    pub description: String,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

impl TaskSnapshot {
    pub fn from_record(record: &TaskRecord) -> Self {
        Self {
            id: record.id.clone(),
            task_type: serde_json::to_value(&record.task_type)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            status: serde_json::to_value(&record.status)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            description: record.description.clone(),
            metadata: record.metadata.clone(),
        }
    }
}

/// Backend event type tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendEventType {
    Ready,
    StateSnapshot,
    TasksSnapshot,
    TranscriptItem,
    AssistantDelta,
    AssistantComplete,
    LineComplete,
    ToolStarted,
    ToolCompleted,
    ClearTranscript,
    ModalRequest,
    SelectRequest,
    Error,
    Shutdown,
}

/// One event sent from the backend to the React frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendEvent {
    pub r#type: BackendEventType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_options: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<TranscriptItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tasks: Option<Vec<TaskSnapshot>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge_sessions: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modal: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// Format permission mode for display.
pub fn format_permission_mode(raw: &str) -> &str {
    match raw {
        "default" | "PermissionMode.DEFAULT" => "Default",
        "plan" | "PermissionMode.PLAN" => "Plan Mode",
        "full_auto" | "PermissionMode.FULL_AUTO" => "Auto",
        other => other,
    }
}

/// Convert AppState to a JSON payload for the frontend.
pub fn state_payload(state: &AppState) -> serde_json::Value {
    serde_json::json!({
        "model": state.model,
        "cwd": state.cwd,
        "provider": state.provider,
        "auth_status": state.auth_status,
        "base_url": state.base_url,
        "permission_mode": format_permission_mode(&state.permission_mode),
        "theme": state.theme,
        "vim_enabled": state.vim_enabled,
        "voice_enabled": state.voice_enabled,
        "voice_available": state.voice_available,
        "voice_reason": state.voice_reason,
        "fast_mode": state.fast_mode,
        "effort": state.effort,
        "passes": state.passes,
        "mcp_connected": state.mcp_connected,
        "mcp_failed": state.mcp_failed,
        "bridge_sessions": state.bridge_sessions,
        "output_style": state.output_style,
        "keybindings": state.keybindings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_permission_mode_default() {
        assert_eq!(format_permission_mode("default"), "Default");
        assert_eq!(format_permission_mode("PermissionMode.DEFAULT"), "Default");
    }

    #[test]
    fn test_format_permission_mode_plan() {
        assert_eq!(format_permission_mode("plan"), "Plan Mode");
        assert_eq!(format_permission_mode("PermissionMode.PLAN"), "Plan Mode");
    }

    #[test]
    fn test_format_permission_mode_full_auto() {
        assert_eq!(format_permission_mode("full_auto"), "Auto");
        assert_eq!(format_permission_mode("PermissionMode.FULL_AUTO"), "Auto");
    }

    #[test]
    fn test_format_permission_mode_unknown() {
        assert_eq!(format_permission_mode("custom"), "custom");
    }

    #[test]
    fn test_frontend_request_submit_line_serde() {
        let req = FrontendRequest::SubmitLine { line: Some("hello".into()) };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"type\":\"submit_line\""));
        let deser: FrontendRequest = serde_json::from_str(&json).unwrap();
        match deser {
            FrontendRequest::SubmitLine { line } => assert_eq!(line, Some("hello".into())),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_frontend_request_shutdown_serde() {
        let req = FrontendRequest::Shutdown;
        let json = serde_json::to_string(&req).unwrap();
        let deser: FrontendRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(deser, FrontendRequest::Shutdown));
    }

    #[test]
    fn test_transcript_item_serde_roundtrip() {
        let item = TranscriptItem {
            role: "assistant".into(),
            text: "hello".into(),
            tool_name: Some("bash".into()),
            tool_input: None,
            is_error: Some(false),
        };
        let json = serde_json::to_string(&item).unwrap();
        let deser: TranscriptItem = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.role, "assistant");
        assert_eq!(deser.tool_name, Some("bash".into()));
    }

    #[test]
    fn test_task_snapshot_from_record() {
        let record = TaskRecord {
            id: "t1".into(),
            task_type: crate::tasks::TaskType::LocalBash,
            status: crate::tasks::TaskStatus::Completed,
            description: "test task".into(),
            cwd: "/tmp".into(),
            output_file: std::path::PathBuf::from("/tmp/out"),
            command: Some("echo hi".into()),
            prompt: None,
            created_at: 0.0,
            started_at: None,
            ended_at: None,
            return_code: Some(0),
            metadata: HashMap::new(),
        };
        let snapshot = TaskSnapshot::from_record(&record);
        assert_eq!(snapshot.id, "t1");
        assert_eq!(snapshot.task_type, "local_bash");
        assert_eq!(snapshot.status, "completed");
        assert_eq!(snapshot.description, "test task");
    }

    #[test]
    fn test_backend_event_type_serde_roundtrip() {
        let et = BackendEventType::Ready;
        let json = serde_json::to_string(&et).unwrap();
        assert_eq!(json, "\"ready\"");
        let deser: BackendEventType = serde_json::from_str(&json).unwrap();
        assert!(matches!(deser, BackendEventType::Ready));
    }

    #[test]
    fn test_state_payload_contains_fields() {
        let state = AppState {
            model: "claude-3".into(),
            permission_mode: "default".into(),
            theme: "dark".into(),
            cwd: "/home".into(),
            provider: "anthropic".into(),
            auth_status: "ok".into(),
            base_url: "".into(),
            vim_enabled: true,
            voice_enabled: false,
            voice_available: false,
            voice_reason: "".into(),
            fast_mode: false,
            effort: "high".into(),
            passes: 2,
            mcp_connected: 1,
            mcp_failed: 0,
            bridge_sessions: 0,
            output_style: "default".into(),
            keybindings: HashMap::new(),
        };
        let payload = state_payload(&state);
        assert_eq!(payload["model"], "claude-3");
        assert_eq!(payload["permission_mode"], "Default");
        assert_eq!(payload["vim_enabled"], true);
        assert_eq!(payload["passes"], 2);
    }
}
