//! List cron jobs tool.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};

pub struct CronListTool;

#[async_trait]
impl crate::traits::Tool for CronListTool {
    fn name(&self) -> &str {
        "CronList"
    }

    fn description(&self) -> &str {
        "List cron jobs"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        true
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let path = oh_config::get_cron_registry_path();
        let jobs = crate::cron_create::read_cron_jobs(&path);

        if jobs.is_empty() {
            return ToolResult::success("No cron jobs configured.");
        }

        let mut output = String::new();
        for job in &jobs {
            output.push_str(&format!(
                "- [{}] schedule={} command={} description={}\n",
                job.id, job.schedule, job.command, job.description
            ));
        }
        ToolResult::success(output.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron_create::{CronJob, write_cron_jobs};
    use crate::traits::Tool;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_cron_list_empty() {
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("OPENHARNESSRS_DATA_DIR", dir.path().to_str().unwrap());
        }

        let tool = CronListTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({}), &ctx).await;

        unsafe { std::env::remove_var("OPENHARNESSRS_DATA_DIR") };

        assert!(!result.is_error);
        assert_eq!(result.output, "No cron jobs configured.");
    }

    #[tokio::test]
    async fn test_cron_list_with_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cron_jobs.json");
        let jobs = vec![CronJob {
            id: "abc123".to_string(),
            schedule: "0 * * * *".to_string(),
            command: "echo hi".to_string(),
            description: "test".to_string(),
        }];
        write_cron_jobs(&path, &jobs).unwrap();

        unsafe {
            std::env::set_var("OPENHARNESSRS_DATA_DIR", dir.path().to_str().unwrap());
        }

        let tool = CronListTool;
        let ctx = ToolExecutionContext::new(PathBuf::from("/tmp"));
        let result = tool.execute(serde_json::json!({}), &ctx).await;

        unsafe { std::env::remove_var("OPENHARNESSRS_DATA_DIR") };

        assert!(!result.is_error);
        assert!(result.output.contains("abc123"));
        assert!(result.output.contains("0 * * * *"));
    }

    #[test]
    fn test_cron_list_is_read_only() {
        let tool = CronListTool;
        assert!(tool.is_read_only(&serde_json::json!({})));
    }
}
