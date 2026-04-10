//! Background task manager: create, list, stop, read output.

use oh_types::tasks::{TaskRecord, TaskStatus, TaskType};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

/// Manages background tasks (shell and agent).
pub struct BackgroundTaskManager {
    tasks: HashMap<String, TaskRecord>,
}

impl BackgroundTaskManager {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
        }
    }

    /// Create a shell task.
    pub async fn create_shell_task(
        &mut self,
        command: &str,
        description: &str,
        cwd: &str,
    ) -> TaskRecord {
        let id = Uuid::new_v4().to_string();
        let output_file = oh_config::get_tasks_dir().join(format!("{id}.log"));

        let record = TaskRecord {
            id: id.clone(),
            task_type: TaskType::LocalBash,
            status: TaskStatus::Pending,
            description: description.to_string(),
            cwd: cwd.to_string(),
            output_file,
            command: Some(command.to_string()),
            prompt: None,
            created_at: now(),
            started_at: None,
            ended_at: None,
            return_code: None,
            metadata: HashMap::new(),
        };

        oh_telemetry::ACTIVE_BACKGROUND_TASKS.add(1, &[]);
        self.tasks.insert(id.clone(), record.clone());

        // TODO: Actually spawn the process
        record
    }

    /// Create an agent task.
    pub async fn create_agent_task(
        &mut self,
        prompt: &str,
        description: &str,
        cwd: &str,
    ) -> TaskRecord {
        let id = Uuid::new_v4().to_string();
        let output_file = oh_config::get_tasks_dir().join(format!("{id}.log"));

        let record = TaskRecord {
            id: id.clone(),
            task_type: TaskType::LocalAgent,
            status: TaskStatus::Pending,
            description: description.to_string(),
            cwd: cwd.to_string(),
            output_file,
            command: None,
            prompt: Some(prompt.to_string()),
            created_at: now(),
            started_at: None,
            ended_at: None,
            return_code: None,
            metadata: HashMap::new(),
        };

        oh_telemetry::ACTIVE_BACKGROUND_TASKS.add(1, &[]);
        self.tasks.insert(id.clone(), record.clone());
        record
    }

    pub fn get_task(&self, id: &str) -> Option<&TaskRecord> {
        self.tasks.get(id)
    }

    pub fn list_tasks(&self, status: Option<TaskStatus>) -> Vec<&TaskRecord> {
        self.tasks
            .values()
            .filter(|t| status.map_or(true, |s| t.status == s))
            .collect()
    }

    pub fn update_task(&mut self, id: &str, description: Option<&str>) -> Option<&TaskRecord> {
        if let Some(task) = self.tasks.get_mut(id) {
            if let Some(desc) = description {
                task.description = desc.to_string();
            }
            Some(task)
        } else {
            None
        }
    }

    pub async fn stop_task(&mut self, id: &str) -> Option<&TaskRecord> {
        if let Some(task) = self.tasks.get_mut(id) {
            task.status = TaskStatus::Killed;
            task.ended_at = Some(now());
            oh_telemetry::ACTIVE_BACKGROUND_TASKS.add(-1, &[]);
            Some(task)
        } else {
            None
        }
    }

    pub async fn read_output(&self, id: &str, max_bytes: usize) -> Result<String, String> {
        let task = self
            .tasks
            .get(id)
            .ok_or_else(|| format!("task not found: {id}"))?;

        if !task.output_file.exists() {
            return Ok(String::new());
        }

        let content = std::fs::read_to_string(&task.output_file)
            .map_err(|e| e.to_string())?;

        if content.len() > max_bytes {
            Ok(content[content.len() - max_bytes..].to_string())
        } else {
            Ok(content)
        }
    }
}

impl Default for BackgroundTaskManager {
    fn default() -> Self {
        Self::new()
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
