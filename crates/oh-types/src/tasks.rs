//! Task data models for background task management.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Type of background task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    LocalBash,
    LocalAgent,
    RemoteAgent,
    InProcessTeammate,
}

/// Status of a background task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Killed,
}

/// Runtime representation of a background task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub task_type: TaskType,
    pub status: TaskStatus,
    pub description: String,
    pub cwd: String,
    pub output_file: PathBuf,
    pub command: Option<String>,
    pub prompt: Option<String>,
    #[serde(default)]
    pub created_at: f64,
    pub started_at: Option<f64>,
    pub ended_at: Option<f64>,
    pub return_code: Option<i32>,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_type_serde_roundtrip() {
        for tt in [TaskType::LocalBash, TaskType::LocalAgent, TaskType::RemoteAgent, TaskType::InProcessTeammate] {
            let json = serde_json::to_string(&tt).unwrap();
            let deser: TaskType = serde_json::from_str(&json).unwrap();
            assert_eq!(deser, tt);
        }
    }

    #[test]
    fn test_task_type_serde_values() {
        assert_eq!(serde_json::to_string(&TaskType::LocalBash).unwrap(), "\"local_bash\"");
        assert_eq!(serde_json::to_string(&TaskType::InProcessTeammate).unwrap(), "\"in_process_teammate\"");
    }

    #[test]
    fn test_task_status_serde_roundtrip() {
        for status in [TaskStatus::Pending, TaskStatus::Running, TaskStatus::Completed, TaskStatus::Failed, TaskStatus::Killed] {
            let json = serde_json::to_string(&status).unwrap();
            let deser: TaskStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(deser, status);
        }
    }

    #[test]
    fn test_task_record_serde_roundtrip() {
        let record = TaskRecord {
            id: "task-1".into(),
            task_type: TaskType::LocalBash,
            status: TaskStatus::Running,
            description: "run tests".into(),
            cwd: "/home/user".into(),
            output_file: PathBuf::from("/tmp/out.log"),
            command: Some("cargo test".into()),
            prompt: None,
            created_at: 1000.0,
            started_at: Some(1001.0),
            ended_at: None,
            return_code: None,
            metadata: HashMap::new(),
        };
        let json = serde_json::to_string(&record).unwrap();
        let deser: TaskRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.id, "task-1");
        assert_eq!(deser.task_type, TaskType::LocalBash);
        assert_eq!(deser.status, TaskStatus::Running);
        assert_eq!(deser.command, Some("cargo test".into()));
    }
}
