//! Execute shell commands tool.
//!
//! Security posture (audit findings TOOL-2, TOOL-9, TOOL-10):
//!
//! * **TOOL-2** — on Linux the spawned child is confined with the Landlock LSM
//!   so it can only *write* under `context.cwd` (plus the system temp dir);
//!   read access is left broad so ordinary commands still work. The login shell
//!   (`-l`) is dropped (no rc sourcing / PATH leakage) and the child runs with a
//!   scrubbed, minimal environment.
//! * **TOOL-9** — stdout/stderr are read through a hard byte cap; if the child
//!   floods output past the cap it is killed and the result truncated.
//! * **TOOL-10** — the child is spawned in its own process group with
//!   `kill_on_drop(true)`; on timeout the entire group is killed (`killpg`) so
//!   grandchildren are reaped rather than orphaned.

use async_trait::async_trait;
use oh_types::tools::{ToolExecutionContext, ToolResult};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tracing::{instrument, warn};

pub struct BashTool;

/// Default command timeout when the caller does not specify one.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// Hard upper bound on a caller-supplied timeout.
const MAX_TIMEOUT_MS: u64 = 600_000;
/// Maximum bytes captured from each of stdout / stderr before the child is
/// killed and the stream truncated (TOOL-9).
const MAX_STREAM_BYTES: usize = 1024 * 1024;
/// Size of each chunk pulled from a child output stream.
const READ_CHUNK_BYTES: usize = 64 * 1024;

#[async_trait]
impl crate::traits::Tool for BashTool {
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "Execute a given bash command and return its output."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Optional timeout in milliseconds (max 600000)"
                }
            },
            "required": ["command"]
        })
    }

    fn is_read_only(&self, _arguments: &serde_json::Value) -> bool {
        false
    }

    /// Bash takes no declared path argument; its `command` is freeform shell.
    /// Filesystem confinement is enforced via the OS sandbox, not path_args.
    fn path_args(&self, _input: &serde_json::Value) -> Vec<String> {
        Vec::new()
    }

    #[instrument(skip(self, context), fields(tool = "Bash"))]
    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let command = match arguments.get("command").and_then(|v| v.as_str()) {
            Some(cmd) => cmd,
            None => return ToolResult::error("Missing required parameter: command"),
        };

        let timeout_ms = arguments
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .min(MAX_TIMEOUT_MS);

        run_command(command, &context.cwd, timeout_ms).await
    }
}

/// Build the bash child command with a scrubbed environment, dropped login
/// shell, its own process group, and (on Linux) a Landlock sandbox applied in
/// the pre-exec hook confining writes to `cwd` + temp.
fn build_command(command: &str, cwd: &PathBuf) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("/bin/bash");
    // Drop `-l`: no login shell, no rc sourcing / PATH leakage (TOOL-2).
    cmd.arg("-c").arg(command);
    cmd.current_dir(cwd);

    // Scrubbed, minimal environment (TOOL-2).
    cmd.env_clear();
    cmd.env("PATH", "/usr/local/bin:/usr/bin:/bin");
    // HOME: forward the parent's HOME when set, else fall back to the per-job
    // cwd. Production stages read-only reference data under the parent HOME
    // (e.g. the cbs-tool catalog at ~/.local/share/cbs-tool/); resetting HOME
    // to cwd silently breaks every tool that resolves config via Path.home().
    // Writes to HOME remain blocked by Landlock + ProtectSystem — tools only
    // READ from it.
    match std::env::var_os("HOME") {
        Some(home) => cmd.env("HOME", home),
        None => cmd.env("HOME", cwd),
    };
    cmd.env("PWD", cwd);
    cmd.env("LANG", "C.UTF-8");
    if let Some(term) = std::env::var_os("TERM") {
        cmd.env("TERM", term);
    }
    // TOOL-2 carve-out: forward a prefix allowlist of NON-SECRET tool-config
    // vars the Capelle CLIs need (e.g. CAPELLE_CHROMA_HTTP routes
    // capelle-beleid to the Chroma server). The host scopes ohrs's own env to
    // a curated allowlist, where the only secrets (CODEX_*, OPENAI_API_KEY) do
    // NOT match these prefixes — so they stay scrubbed from agent-controlled
    // bash. Never widen this to a broad passthrough.
    for (key, val) in std::env::vars_os() {
        if let Some(k) = key.to_str() {
            if k.starts_with("CAPELLE_") || k.starts_with("CBS_") {
                cmd.env(&key, &val);
            }
        }
    }

    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Own process group so the whole tree can be killed on timeout (TOOL-10).
    cmd.process_group(0);

    // Kill the child if the future is dropped (e.g. cancellation) (TOOL-10).
    cmd.kill_on_drop(true);

    #[cfg(target_os = "linux")]
    {
        let cwd = cwd.clone();
        // SAFETY: the closure runs in the forked child before exec. It only
        // calls async-signal-safe-ish Landlock syscalls; on any error it logs
        // (best effort) and lets the process continue UNCONFINED is NOT done —
        // we fail closed by returning an error from pre_exec so the spawn aborts.
        unsafe {
            cmd.pre_exec(move || apply_landlock_sandbox(&cwd));
        }
    }

    cmd
}

/// Apply a Landlock ruleset confining filesystem WRITES to `cwd` and the system
/// temp dir, while leaving READ access broad (so commands can read libraries,
/// configs, etc.). Returns an `io::Error` on failure so the spawn fails closed.
#[cfg(target_os = "linux")]
fn apply_landlock_sandbox(cwd: &std::path::Path) -> std::io::Result<()> {
    use landlock::{
        Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr, ABI,
    };

    let abi = ABI::V1;
    // Read-everything bitset and write-everything bitset for this ABI.
    let read_access = AccessFs::from_read(abi);
    let write_access = AccessFs::from_all(abi);

    let mk_err = |msg: String| std::io::Error::new(std::io::ErrorKind::Other, msg);

    let mut ruleset = Ruleset::default()
        .handle_access(write_access)
        .map_err(|e| mk_err(format!("landlock handle_access: {e}")))?
        .create()
        .map_err(|e| mk_err(format!("landlock create: {e}")))?;

    // Allow full (read+write) access beneath cwd.
    if let Ok(fd) = PathFd::new(cwd) {
        ruleset = ruleset
            .add_rule(PathBeneath::new(fd, write_access))
            .map_err(|e| mk_err(format!("landlock add cwd rule: {e}")))?;
    }

    // Allow full access beneath the system temp dir (build tools, mktemp, etc.).
    let tmp = std::env::temp_dir();
    if let Ok(fd) = PathFd::new(&tmp) {
        ruleset = ruleset
            .add_rule(PathBeneath::new(fd, write_access))
            .map_err(|e| mk_err(format!("landlock add tmp rule: {e}")))?;
    }

    // Allow read-only access to the rest of the filesystem root so ordinary
    // commands keep working (shared libs, /usr, /etc reads).
    if let Ok(fd) = PathFd::new("/") {
        ruleset = ruleset
            .add_rule(PathBeneath::new(fd, read_access))
            .map_err(|e| mk_err(format!("landlock add root-read rule: {e}")))?;
    }

    ruleset
        .restrict_self()
        .map_err(|e| mk_err(format!("landlock restrict_self: {e}")))?;

    Ok(())
}

/// Spawn the command, enforce the timeout, kill the process group on timeout,
/// and read bounded stdout/stderr.
async fn run_command(command: &str, cwd: &PathBuf, timeout_ms: u64) -> ToolResult {
    let mut cmd = build_command(command, cwd);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("Failed to execute command: {e}")),
    };

    let pid = child.id();

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    // Read both streams concurrently (each stops at the byte cap), bounded by
    // the timeout. The moment EITHER stream caps we kill the process group so a
    // flooding child cannot block on a full pipe and starve the other stream's
    // EOF (TOOL-9 + TOOL-10).
    let killed_on_cap = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let kc_out = killed_on_cap.clone();
    let kc_err = killed_on_cap.clone();

    let out_fut = async {
        let r = read_capped(&mut stdout_pipe).await;
        if r.1 && !kc_out.swap(true, std::sync::atomic::Ordering::SeqCst) {
            kill_process_group(pid);
        }
        r
    };
    let err_fut = async {
        let r = read_capped(&mut stderr_pipe).await;
        if r.1 && !kc_err.swap(true, std::sync::atomic::Ordering::SeqCst) {
            kill_process_group(pid);
        }
        r
    };

    let read_fut = async { tokio::join!(out_fut, err_fut) };

    let read_result =
        tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), read_fut).await;

    let ((stdout, out_capped), (stderr, err_capped)) = match read_result {
        Ok(pair) => pair,
        Err(_) => {
            // Timeout while reading: kill the whole group to reap grandchildren.
            kill_process_group(pid);
            let _ = child.kill().await;
            return ToolResult::error(format!("Command timed out after {timeout_ms}ms"));
        }
    };

    if out_capped || err_capped {
        // Output overflowed the cap (TOOL-9): the group was already signalled
        // above; reap the child and report the truncated output.
        let _ = child.kill().await;
        let mut combined = combine(&stdout, &stderr);
        combined.push_str("\n...[truncated: output byte cap reached]");
        return ToolResult::success(combined);
    }

    // Streams closed within the cap; reap the child (bounded by remaining time).
    let status = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        child.wait(),
    )
    .await;

    let combined = combine(&stdout, &stderr);
    match status {
        Ok(Ok(s)) if s.success() => ToolResult::success(combined),
        Ok(Ok(s)) => ToolResult::error(format!(
            "Command exited with code {}\n{}",
            s.code().unwrap_or(-1),
            combined
        )),
        Ok(Err(e)) => ToolResult::error(format!("Failed to wait for command: {e}")),
        Err(_) => {
            kill_process_group(pid);
            let _ = child.kill().await;
            ToolResult::error(format!("Command timed out after {timeout_ms}ms"))
        }
    }
}

/// Combine stdout and stderr into a single human-readable string.
fn combine(stdout: &str, stderr: &str) -> String {
    if stderr.is_empty() {
        stdout.to_string()
    } else if stdout.is_empty() {
        stderr.to_string()
    } else {
        format!("{stdout}\n{stderr}")
    }
}

/// Read up to [`MAX_STREAM_BYTES`] from a child pipe, returning the decoded text
/// and whether the cap was hit (TOOL-9). Reads in chunks and stops once capped
/// rather than buffering the entire stream.
async fn read_capped<R>(pipe: &mut Option<R>) -> (String, bool)
where
    R: AsyncReadExt + Unpin,
{
    let reader = match pipe {
        Some(r) => r,
        None => return (String::new(), false),
    };

    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; READ_CHUNK_BYTES];
    let mut capped = false;

    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                let remaining = MAX_STREAM_BYTES.saturating_sub(buf.len());
                if n >= remaining {
                    // Reached (or would exceed) the cap: keep only what fits and
                    // stop. There is at least as much data as the remaining
                    // budget, so the stream is considered truncated.
                    buf.extend_from_slice(&chunk[..remaining]);
                    capped = true;
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(_) => break,
        }
    }

    (String::from_utf8_lossy(&buf).to_string(), capped)
}

/// Kill the entire process group led by `pid` (TOOL-10). The child was spawned
/// with `process_group(0)`, so its PID is its PGID.
fn kill_process_group(pid: Option<u32>) {
    #[cfg(target_os = "linux")]
    {
        use nix::sys::signal::{killpg, Signal};
        use nix::unistd::Pid;
        if let Some(pid) = pid {
            let pgid = Pid::from_raw(pid as i32);
            if let Err(e) = killpg(pgid, Signal::SIGKILL) {
                warn!(pid, error = %e, "failed to kill bash process group");
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::Tool;
    use oh_types::tools::ToolExecutionContext;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext::new(std::env::current_dir().unwrap())
    }

    #[tokio::test]
    async fn test_execute_echo_hello() {
        let tool = BashTool;
        let result = tool
            .execute(serde_json::json!({"command": "echo hello"}), &ctx())
            .await;
        assert!(!result.is_error);
        assert!(result.output.contains("hello"));
    }

    #[tokio::test]
    async fn test_execute_exit_1_is_error() {
        let tool = BashTool;
        let result = tool
            .execute(serde_json::json!({"command": "exit 1"}), &ctx())
            .await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn test_execute_with_custom_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let context = ToolExecutionContext::new(dir.path().to_path_buf());
        let tool = BashTool;
        let result = tool
            .execute(serde_json::json!({"command": "pwd"}), &context)
            .await;
        assert!(!result.is_error);
        let expected = std::fs::canonicalize(dir.path()).unwrap();
        let actual_trimmed = result.output.trim();
        let actual =
            std::fs::canonicalize(actual_trimmed).unwrap_or_else(|_| actual_trimmed.into());
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn test_env_allowlist_forwards_home_and_capelle_but_scrubs_rest() {
        // Regression (capelle): env_clear() (TOOL-2) wiped HOME and
        // CAPELLE_CHROMA_HTTP, silently breaking the cbs (catalog under
        // ~/.local/share) and capelle-beleid (CAPELLE_CHROMA_HTTP routing)
        // CLIs. HOME + CAPELLE_*/CBS_* must reach the command; everything else
        // (notably CODEX_*/OPENAI_API_KEY secrets) must stay scrubbed.
        let home = tempfile::tempdir().unwrap();
        let home_path = home.path().to_string_lossy().to_string();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home_path);
        std::env::set_var("CAPELLE_OHRS_ENVTEST", "chroma-host-42");
        std::env::set_var("OHRS_ENVTEST_NOTALLOWED", "leaky-secret");
        let tool = BashTool;
        let result = tool
            .execute(
                serde_json::json!({"command":
                    "printf 'HOME=%s CAP=%s OTHER=%s' \"$HOME\" \"$CAPELLE_OHRS_ENVTEST\" \"$OHRS_ENVTEST_NOTALLOWED\""}),
                &ctx(),
            )
            .await;
        std::env::remove_var("CAPELLE_OHRS_ENVTEST");
        std::env::remove_var("OHRS_ENVTEST_NOTALLOWED");
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        assert!(!result.is_error, "{}", result.output);
        assert!(
            result.output.contains(&format!("HOME={home_path}")),
            "parent HOME should be forwarded, got: {}",
            result.output
        );
        assert!(
            result.output.contains("CAP=chroma-host-42"),
            "CAPELLE_* tool-config var should be forwarded, got: {}",
            result.output
        );
        assert!(
            !result.output.contains("leaky-secret"),
            "non-allowlisted var must stay scrubbed, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn test_execute_missing_command_arg() {
        let tool = BashTool;
        let result = tool.execute(serde_json::json!({}), &ctx()).await;
        assert!(result.is_error);
        assert!(result.output.contains("command"));
    }

    #[tokio::test]
    async fn test_timeout_kills_command() {
        let tool = BashTool;
        let result = tool
            .execute(
                serde_json::json!({"command": "sleep 30", "timeout": 200}),
                &ctx(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.output.contains("timed out"));
    }

    #[tokio::test]
    async fn test_output_is_capped() {
        let tool = BashTool;
        // Emit far more than MAX_STREAM_BYTES; expect truncation, not OOM.
        let result = tool
            .execute(
                serde_json::json!({"command": "yes x | head -c 5000000", "timeout": 20000}),
                &ctx(),
            )
            .await;
        assert!(result.output.len() <= MAX_STREAM_BYTES + 256);
        assert!(result.output.contains("truncated"));
    }

    #[tokio::test]
    async fn test_write_outside_cwd_blocked_when_landlock_available() {
        // On a kernel with Landlock enabled, writing outside cwd/tmp fails. On
        // kernels without it the spawn fails closed (pre_exec error). Either
        // way the command must NOT succeed in creating /etc/oh_test_probe.
        let dir = tempfile::tempdir().unwrap();
        let context = ToolExecutionContext::new(dir.path().to_path_buf());
        let tool = BashTool;
        let _ = tool
            .execute(
                serde_json::json!({"command": "echo probe > /oh_landlock_probe 2>/dev/null; echo done"}),
                &context,
            )
            .await;
        // Probe file must not exist at the filesystem root.
        assert!(!std::path::Path::new("/oh_landlock_probe").exists());
    }

    #[test]
    fn test_schema_has_required_command() {
        let tool = BashTool;
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "command"));
    }

    #[test]
    fn test_is_read_only_returns_false() {
        let tool = BashTool;
        assert!(!tool.is_read_only(&serde_json::json!({})));
    }

    #[test]
    fn test_path_args_is_empty() {
        let tool = BashTool;
        assert!(tool.path_args(&serde_json::json!({"command": "ls"})).is_empty());
    }
}
