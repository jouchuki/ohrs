//! Delete a cron job tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct CronDeleteTool;

#[async_trait]
impl crate::traits::Tool for CronDeleteTool {
    fn name(&self) -> &str {
        "CronDelete"
    }

    fn description(&self) -> &str {
        "Delete a cron job"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "Cron job ID to delete"}
            },
            "required": ["id"]
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
        let id = match arguments.get("id").and_then(|v| v.as_str()) {
            Some(i) => i,
            None => return ToolResult::error("Missing required parameter: id"),
        };

        let path = oh_config::get_cron_registry_path();
        let mut jobs = crate::cron_create::read_cron_jobs(&path);

        let original_len = jobs.len();
        jobs.retain(|job| job.id != id);

        if jobs.len() == original_len {
            return ToolResult::error(format!("Cron job '{id}' not found"));
        }

        match crate::cron_create::write_cron_jobs(&path, &jobs) {
            Ok(()) => ToolResult::success(format!("Deleted cron job: {id}")),
            Err(e) => ToolResult::error(format!("Failed to write cron jobs: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron_create::{CronJob, write_cron_jobs, read_cron_jobs};
    use crate::traits::Tool;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_cron_delete_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cron_jobs.json");
        let jobs = vec![CronJob {
            id: "del-me".to_string(),
            schedule: "0 * * * *".to_string(),
            command: "echo hi".to_string(),
            description: "".to_string(),
        }];
        write_cron_jobs(&path, &jobs).unwrap();

        unsafe {
            std::env::set_var("OPENHARNESSRS_DATA_DIR", dir.path().to_str().unwrap());
        }

        let tool = CronDeleteTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool
            .execute(serde_json::json!({"id": "del-me"}), &ctx)
            .await;

        unsafe { std::env::remove_var("OPENHARNESSRS_DATA_DIR") };

        assert!(!result.is_error, "Expected success, got: {}", result.output);
        assert_eq!(result.output, "Deleted cron job: del-me");

        // Verify file now empty
        let remaining = read_cron_jobs(&path);
        assert!(remaining.is_empty());
    }

    #[tokio::test]
    async fn test_cron_delete_not_found() {
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("OPENHARNESSRS_DATA_DIR", dir.path().to_str().unwrap());
        }

        let tool = CronDeleteTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool
            .execute(serde_json::json!({"id": "nonexistent"}), &ctx)
            .await;

        unsafe { std::env::remove_var("OPENHARNESSRS_DATA_DIR") };

        assert!(result.is_error);
        assert!(result.output.contains("not found"));
    }

    #[tokio::test]
    async fn test_cron_delete_missing_id() {
        let tool = CronDeleteTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_error);
    }

    #[test]
    fn test_cron_delete_name() {
        let tool = CronDeleteTool;
        assert_eq!(tool.name(), "CronDelete");
    }
}
