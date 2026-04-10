//! Create a cron job tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A single cron job entry stored in cron_jobs.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub schedule: String,
    pub command: String,
    pub description: String,
}

/// Read existing cron jobs from a file path.
pub fn read_cron_jobs(path: &Path) -> Vec<CronJob> {
    match std::fs::read_to_string(path) {
        Ok(content) if !content.trim().is_empty() => {
            serde_json::from_str(&content).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// Write cron jobs to a file path.
pub fn write_cron_jobs(path: &Path, jobs: &[CronJob]) -> Result<(), String> {
    let content = serde_json::to_string_pretty(jobs).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, content).map_err(|e| e.to_string())
}

pub struct CronCreateTool;

#[async_trait]
impl crate::traits::Tool for CronCreateTool {
    fn name(&self) -> &str {
        "CronCreate"
    }

    fn description(&self) -> &str {
        "Create a cron job"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "schedule": {"type": "string", "description": "Cron expression"},
                "command": {"type": "string", "description": "Command to run"},
                "description": {"type": "string", "description": "Optional description"}
            },
            "required": ["schedule", "command"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        false
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let schedule = match arguments.get("schedule").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::error("Missing required parameter: schedule"),
        };
        let command = match arguments.get("command").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolResult::error("Missing required parameter: command"),
        };
        let description = arguments
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let path = oh_config::get_cron_registry_path();
        let mut jobs = read_cron_jobs(&path);

        let id = uuid::Uuid::new_v4().to_string();
        jobs.push(CronJob {
            id: id.clone(),
            schedule: schedule.to_string(),
            command: command.to_string(),
            description: description.to_string(),
        });

        match write_cron_jobs(&path, &jobs) {
            Ok(()) => ToolResult::success(format!("Created cron job: {id}")),
            Err(e) => ToolResult::error(format!("Failed to write cron jobs: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_cron_create_success() {
        let dir = tempfile::tempdir().unwrap();
        let cron_path = dir.path().join("cron_jobs.json");
        unsafe {
            std::env::set_var("OPENHARNESSRS_DATA_DIR", dir.path().to_str().unwrap());
        }

        let tool = CronCreateTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool
            .execute(
                serde_json::json!({"schedule": "* * * * *", "command": "echo hello"}),
                &ctx,
            )
            .await;

        unsafe { std::env::remove_var("OPENHARNESSRS_DATA_DIR") };

        assert!(!result.is_error, "Expected success, got: {}", result.output);
        assert!(result.output.starts_with("Created cron job: "));

        // Verify file was written
        let jobs: Vec<CronJob> = serde_json::from_str(&std::fs::read_to_string(&cron_path).unwrap()).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].schedule, "* * * * *");
        assert_eq!(jobs[0].command, "echo hello");
    }

    #[tokio::test]
    async fn test_cron_create_missing_schedule() {
        let tool = CronCreateTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool
            .execute(serde_json::json!({"command": "echo hello"}), &ctx)
            .await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn test_cron_create_missing_command() {
        let tool = CronCreateTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool
            .execute(serde_json::json!({"schedule": "* * * * *"}), &ctx)
            .await;
        assert!(result.is_error);
    }

    #[test]
    fn test_cron_create_name() {
        let tool = CronCreateTool;
        assert_eq!(tool.name(), "CronCreate");
    }

    #[test]
    fn test_read_write_cron_jobs_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cron_jobs.json");

        let jobs = vec![CronJob {
            id: "test-id".to_string(),
            schedule: "0 * * * *".to_string(),
            command: "ls".to_string(),
            description: "hourly ls".to_string(),
        }];
        write_cron_jobs(&path, &jobs).unwrap();

        let loaded = read_cron_jobs(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "test-id");
        assert_eq!(loaded[0].schedule, "0 * * * *");
    }

    #[test]
    fn test_read_cron_jobs_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let jobs = read_cron_jobs(&path);
        assert!(jobs.is_empty());
    }
}
