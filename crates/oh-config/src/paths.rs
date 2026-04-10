//! Path resolution for OpenHarness configuration and data directories.
//!
//! Follows XDG-like conventions with `~/.openharnessrs/` as the default base.

use std::path::{Path, PathBuf};

const DEFAULT_BASE_DIR: &str = ".openharnessrs";
const CONFIG_FILE_NAME: &str = "settings.json";

/// Return the configuration directory, creating it if needed.
pub fn get_config_dir() -> PathBuf {
    let dir = if let Ok(env_dir) = std::env::var("OPENHARNESSRS_CONFIG_DIR") {
        PathBuf::from(env_dir)
    } else if let Ok(env_dir) = std::env::var("OPENHARNESS_CONFIG_DIR") {
        PathBuf::from(env_dir)
    } else {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(DEFAULT_BASE_DIR)
    };
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Return the path to the main settings file.
pub fn get_config_file_path() -> PathBuf {
    get_config_dir().join(CONFIG_FILE_NAME)
}

/// Return the data directory.
pub fn get_data_dir() -> PathBuf {
    let dir = if let Ok(env_dir) = std::env::var("OPENHARNESSRS_DATA_DIR") {
        PathBuf::from(env_dir)
    } else if let Ok(env_dir) = std::env::var("OPENHARNESS_DATA_DIR") {
        PathBuf::from(env_dir)
    } else {
        get_config_dir().join("data")
    };
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Return the logs directory.
pub fn get_logs_dir() -> PathBuf {
    let dir = if let Ok(env_dir) = std::env::var("OPENHARNESSRS_LOGS_DIR") {
        PathBuf::from(env_dir)
    } else if let Ok(env_dir) = std::env::var("OPENHARNESS_LOGS_DIR") {
        PathBuf::from(env_dir)
    } else {
        get_config_dir().join("logs")
    };
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Return the session storage directory.
pub fn get_sessions_dir() -> PathBuf {
    let dir = get_data_dir().join("sessions");
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Return the background task output directory.
pub fn get_tasks_dir() -> PathBuf {
    let dir = get_data_dir().join("tasks");
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Return the feedback storage directory.
pub fn get_feedback_dir() -> PathBuf {
    let dir = get_data_dir().join("feedback");
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Return the feedback log file path.
pub fn get_feedback_log_path() -> PathBuf {
    get_feedback_dir().join("feedback.log")
}

/// Return the cron registry file path.
pub fn get_cron_registry_path() -> PathBuf {
    get_data_dir().join("cron_jobs.json")
}

/// Return the per-project .openharnessrs directory.
pub fn get_project_config_dir(cwd: &Path) -> PathBuf {
    let dir = cwd.join(".openharnessrs");
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Return the per-project issue context file.
pub fn get_project_issue_file(cwd: &Path) -> PathBuf {
    get_project_config_dir(cwd).join("issue.md")
}

/// Return the per-project PR comments context file.
pub fn get_project_pr_comments_file(cwd: &Path) -> PathBuf {
    get_project_config_dir(cwd).join("pr_comments.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── env-based overrides ──────────────────────────────────────

    #[test]
    fn test_get_config_dir_env_override() {
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join("custom_config");
        unsafe { std::env::set_var("OPENHARNESSRS_CONFIG_DIR", custom.to_str().unwrap()) };
        let result = get_config_dir();
        unsafe { std::env::remove_var("OPENHARNESSRS_CONFIG_DIR") };
        assert_eq!(result, custom);
        assert!(result.is_dir());
    }

    #[test]
    fn test_get_data_dir_env_override() {
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join("custom_data");
        unsafe { std::env::set_var("OPENHARNESSRS_DATA_DIR", custom.to_str().unwrap()) };
        let result = get_data_dir();
        unsafe { std::env::remove_var("OPENHARNESSRS_DATA_DIR") };
        assert_eq!(result, custom);
        assert!(result.is_dir());
    }

    #[test]
    fn test_get_logs_dir_env_override() {
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join("custom_logs");
        unsafe { std::env::set_var("OPENHARNESSRS_LOGS_DIR", custom.to_str().unwrap()) };
        let result = get_logs_dir();
        unsafe { std::env::remove_var("OPENHARNESSRS_LOGS_DIR") };
        assert_eq!(result, custom);
        assert!(result.is_dir());
    }

    // ── OPENHARNESS_* fallback env vars ──────────────────────────

    #[test]
    fn test_get_config_dir_legacy_env_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join("legacy_config");
        unsafe {
            std::env::remove_var("OPENHARNESSRS_CONFIG_DIR");
            std::env::set_var("OPENHARNESS_CONFIG_DIR", custom.to_str().unwrap());
        }
        let result = get_config_dir();
        unsafe { std::env::remove_var("OPENHARNESS_CONFIG_DIR") };
        assert_eq!(result, custom);
        assert!(result.is_dir());
    }

    #[test]
    fn test_get_config_dir_primary_takes_precedence_over_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("primary_config");
        let legacy = dir.path().join("legacy_config");
        unsafe {
            std::env::set_var("OPENHARNESSRS_CONFIG_DIR", primary.to_str().unwrap());
            std::env::set_var("OPENHARNESS_CONFIG_DIR", legacy.to_str().unwrap());
        }
        let result = get_config_dir();
        unsafe {
            std::env::remove_var("OPENHARNESSRS_CONFIG_DIR");
            std::env::remove_var("OPENHARNESS_CONFIG_DIR");
        }
        assert_eq!(result, primary);
    }

    #[test]
    fn test_get_data_dir_legacy_env_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join("legacy_data");
        unsafe {
            std::env::remove_var("OPENHARNESSRS_DATA_DIR");
            std::env::set_var("OPENHARNESS_DATA_DIR", custom.to_str().unwrap());
        }
        let result = get_data_dir();
        unsafe { std::env::remove_var("OPENHARNESS_DATA_DIR") };
        assert_eq!(result, custom);
        assert!(result.is_dir());
    }

    #[test]
    fn test_get_logs_dir_legacy_env_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join("legacy_logs");
        unsafe {
            std::env::remove_var("OPENHARNESSRS_LOGS_DIR");
            std::env::set_var("OPENHARNESS_LOGS_DIR", custom.to_str().unwrap());
        }
        let result = get_logs_dir();
        unsafe { std::env::remove_var("OPENHARNESS_LOGS_DIR") };
        assert_eq!(result, custom);
        assert!(result.is_dir());
    }

    // ── default paths (use OPENHARNESSRS_CONFIG_DIR to control base) ──

    #[test]
    fn test_get_config_dir_default_structure() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("oh_base");
        unsafe { std::env::set_var("OPENHARNESSRS_CONFIG_DIR", base.to_str().unwrap()) };
        let result = get_config_dir();
        unsafe { std::env::remove_var("OPENHARNESSRS_CONFIG_DIR") };
        assert_eq!(result, base);
        assert!(result.is_dir());
    }

    #[test]
    fn test_get_config_file_path_ends_with_settings_json() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("oh_cfg");
        unsafe { std::env::set_var("OPENHARNESSRS_CONFIG_DIR", base.to_str().unwrap()) };
        let result = get_config_file_path();
        unsafe { std::env::remove_var("OPENHARNESSRS_CONFIG_DIR") };
        assert_eq!(result, base.join("settings.json"));
    }

    #[test]
    fn test_get_data_dir_defaults_under_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("oh_cfg2");
        unsafe {
            std::env::set_var("OPENHARNESSRS_CONFIG_DIR", base.to_str().unwrap());
            std::env::remove_var("OPENHARNESSRS_DATA_DIR");
        }
        let result = get_data_dir();
        unsafe { std::env::remove_var("OPENHARNESSRS_CONFIG_DIR") };
        assert_eq!(result, base.join("data"));
        assert!(result.is_dir());
    }

    #[test]
    fn test_get_logs_dir_defaults_under_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("oh_cfg3");
        unsafe {
            std::env::set_var("OPENHARNESSRS_CONFIG_DIR", base.to_str().unwrap());
            std::env::remove_var("OPENHARNESSRS_LOGS_DIR");
        }
        let result = get_logs_dir();
        unsafe { std::env::remove_var("OPENHARNESSRS_CONFIG_DIR") };
        assert_eq!(result, base.join("logs"));
        assert!(result.is_dir());
    }

    #[test]
    fn test_get_sessions_dir_under_data() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("oh_data");
        unsafe { std::env::set_var("OPENHARNESSRS_DATA_DIR", data.to_str().unwrap()) };
        let result = get_sessions_dir();
        unsafe { std::env::remove_var("OPENHARNESSRS_DATA_DIR") };
        assert_eq!(result, data.join("sessions"));
        assert!(result.is_dir());
    }

    #[test]
    fn test_get_tasks_dir_under_data() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("oh_data2");
        unsafe { std::env::set_var("OPENHARNESSRS_DATA_DIR", data.to_str().unwrap()) };
        let result = get_tasks_dir();
        unsafe { std::env::remove_var("OPENHARNESSRS_DATA_DIR") };
        assert_eq!(result, data.join("tasks"));
        assert!(result.is_dir());
    }

    #[test]
    fn test_get_feedback_dir_under_data() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("oh_data3");
        unsafe { std::env::set_var("OPENHARNESSRS_DATA_DIR", data.to_str().unwrap()) };
        let result = get_feedback_dir();
        unsafe { std::env::remove_var("OPENHARNESSRS_DATA_DIR") };
        assert_eq!(result, data.join("feedback"));
        assert!(result.is_dir());
    }

    #[test]
    fn test_get_feedback_log_path() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("oh_data4");
        unsafe { std::env::set_var("OPENHARNESSRS_DATA_DIR", data.to_str().unwrap()) };
        let result = get_feedback_log_path();
        unsafe { std::env::remove_var("OPENHARNESSRS_DATA_DIR") };
        assert_eq!(result, data.join("feedback").join("feedback.log"));
    }

    #[test]
    fn test_get_cron_registry_path() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("oh_data5");
        unsafe { std::env::set_var("OPENHARNESSRS_DATA_DIR", data.to_str().unwrap()) };
        let result = get_cron_registry_path();
        unsafe { std::env::remove_var("OPENHARNESSRS_DATA_DIR") };
        assert_eq!(result, data.join("cron_jobs.json"));
    }

    // ── project-level paths ──────────────────────────────────────

    #[test]
    fn test_get_project_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = get_project_config_dir(dir.path());
        assert_eq!(result, dir.path().join(".openharnessrs"));
        assert!(result.is_dir());
    }

    #[test]
    fn test_get_project_issue_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = get_project_issue_file(dir.path());
        assert_eq!(result, dir.path().join(".openharnessrs").join("issue.md"));
    }

    #[test]
    fn test_get_project_pr_comments_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = get_project_pr_comments_file(dir.path());
        assert_eq!(
            result,
            dir.path().join(".openharnessrs").join("pr_comments.md")
        );
    }
}
