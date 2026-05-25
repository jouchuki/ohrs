//! Runtime environment snapshot for injecting context into the system prompt.

use std::path::Path;
use std::process::Command;

pub struct EnvironmentInfo {
    pub os_name: String,
    pub shell: String,
    pub cwd: String,
    pub date: String,
    pub rust_version: String,
    pub is_git_repo: bool,
    pub git_branch: Option<String>,
}

/// Detect the running shell from $SHELL, falling back to "unknown".
fn detect_shell() -> String {
    std::env::var("SHELL")
        .map(|s| {
            Path::new(&s)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_owned()
        })
        .unwrap_or_else(|_| "unknown".into())
}

/// Return (is_git_repo, branch_name) for the given directory.
fn detect_git_info(cwd: &Path) -> (bool, Option<String>) {
    let inside = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output();
    let is_git = inside
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false);
    if !is_git {
        return (false, None);
    }
    let branch = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        });
    (true, branch)
}

/// Detect the installed Rust toolchain version via `rustc --version`.
fn detect_rust_version() -> String {
    Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        })
        .unwrap_or_else(|| "unknown".into())
}

/// Gather a snapshot of the current environment.
pub fn gather(cwd: &Path) -> EnvironmentInfo {
    let os_name = std::env::consts::OS.to_owned();
    let shell = detect_shell();
    let cwd_str = cwd.display().to_string();
    let date = chrono_or_fallback();
    let rust_version = detect_rust_version();
    let (is_git_repo, git_branch) = detect_git_info(cwd);

    EnvironmentInfo {
        os_name,
        shell,
        cwd: cwd_str,
        date,
        rust_version,
        is_git_repo,
        git_branch,
    }
}

/// Format the environment as a markdown section for the system prompt.
pub fn format_section(env: &EnvironmentInfo) -> String {
    let mut lines = vec![
        "# Environment".to_owned(),
        format!("- OS: {}", env.os_name),
        format!("- Shell: {}", env.shell),
        format!("- Working directory: {}", env.cwd),
        format!("- Date: {}", env.date),
        format!("- Rust: {}", env.rust_version),
    ];
    if env.is_git_repo {
        let mut git_line = "- Git: yes".to_owned();
        if let Some(ref branch) = env.git_branch {
            git_line.push_str(&format!(" (branch: {branch})"));
        }
        lines.push(git_line);
    } else {
        lines.push("- Git: no".to_owned());
    }
    lines.join("\n")
}

/// Use the `date` command or a static fallback when chrono is unavailable.
fn chrono_or_fallback() -> String {
    // Try `date +%Y-%m-%d` as a zero-dep approach.
    Command::new("date")
        .arg("+%Y-%m-%d")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        })
        .unwrap_or_else(|| "unknown".into())
}
