//! Incremental, versioned trajectory writer (audit contract **C1**).
//!
//! ohrs streams one JSON object per line to the trajectory file, calling
//! [`flush`](std::io::Write::flush) after every line so a crash, `SIGTERM`, or
//! `max_turns` abort still leaves a parseable, durable record. The final
//! `{"kind":"end",...}` line is written even on an abnormal exit via a [`Drop`]
//! guard, and a `SIGTERM` handler (installed in `main`) finalizes the
//! process-wide writer before the runtime tears down.
//!
//! Schema (every line carries `"v": SCHEMA_VERSION`; `kind` discriminates):
//!
//! ```jsonl
//! {"v":1,"kind":"meta","ts":"<rfc3339>","model":"<str>","session_id":"<str>"}
//! {"v":1,"kind":"assistant","turn":<int>,"seq":<int>,"text":"<str>","tool_calls":[{"id":"<str>","name":"<str>","arguments":<object>}]}
//! {"v":1,"kind":"tool_result","turn":<int>,"seq":<int>,"tool_use_id":"<str>","name":"<str>","content":"<str>","is_error":<bool>}
//! {"v":1,"kind":"usage","turn":<int>,"input_tokens":<int>,"output_tokens":<int>,"cache_read_input_tokens":<int>,"cache_creation_input_tokens":<int>}
//! {"v":1,"kind":"end","turn":<int>,"status":"ok"|"error"|"max_turns","error":<str|null>}
//! ```
//!
//! `seq` is monotonically increasing across the whole file; `tool_calls` may be
//! empty (`[]`).

use std::io::Write;
use std::sync::{Arc, Mutex};

use oh_types::api::UsageSnapshot;

/// Schema version stamped on every trajectory line. Bump on any breaking change
/// to the line shapes; capelle keys its parser off this.
pub const SCHEMA_VERSION: u32 = 1;

/// Terminal run status recorded on the final `end` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// The run completed normally.
    Ok,
    /// The run hit the configured turn ceiling.
    MaxTurns,
    /// The run failed (API error, interrupted, crashed, SIGTERM).
    Error,
}

impl RunStatus {
    /// The wire string used in the `end` line (contract C1 / C2).
    pub const fn as_str(self) -> &'static str {
        match self {
            RunStatus::Ok => "ok",
            RunStatus::MaxTurns => "max_turns",
            RunStatus::Error => "error",
        }
    }
}

/// Incremental JSONL trajectory writer. Each `write_*` call serializes one line
/// and flushes immediately so partial runs are durable.
pub struct TrajectoryWriter {
    writer: std::io::BufWriter<std::fs::File>,
    /// Monotonic across the whole file.
    seq: u64,
    /// Current turn index; advanced by each assistant turn.
    turn: u64,
    /// Set once the `end` line has been written so [`Drop`] does not double-write.
    ended: bool,
    /// Path, for diagnostics only.
    path: String,
}

impl TrajectoryWriter {
    /// Create the trajectory file and write the `meta` line.
    pub fn create(
        path: &str,
        model: &str,
        session_id: &str,
    ) -> Result<Self, std::io::Error> {
        let file = std::fs::File::create(path)?;
        let mut w = Self {
            writer: std::io::BufWriter::new(file),
            seq: 0,
            turn: 0,
            ended: false,
            path: path.to_string(),
        };
        w.write_line(serde_json::json!({
            "v": SCHEMA_VERSION,
            "kind": "meta",
            "ts": rfc3339_now(),
            "model": model,
            "session_id": session_id,
        }))?;
        Ok(w)
    }

    /// Serialize `entry` as one line and flush.
    fn write_line(&mut self, entry: serde_json::Value) -> Result<(), std::io::Error> {
        serde_json::to_writer(&mut self.writer, &entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        self.writer.write_all(b"\n")?;
        // Durable per-line flush: the whole point of the incremental writer.
        self.writer.flush()?;
        Ok(())
    }

    /// Begin a new turn, returning its index. The first turn is `1`.
    fn next_turn(&mut self) -> u64 {
        self.turn += 1;
        self.turn
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// Record a completed assistant turn (final message text + tool calls).
    pub fn write_assistant(
        &mut self,
        text: &str,
        tool_calls: &[serde_json::Value],
    ) -> Result<u64, std::io::Error> {
        let turn = self.next_turn();
        let seq = self.next_seq();
        self.write_line(serde_json::json!({
            "v": SCHEMA_VERSION,
            "kind": "assistant",
            "turn": turn,
            "seq": seq,
            "text": text,
            "tool_calls": tool_calls,
        }))?;
        Ok(turn)
    }

    /// Record a tool result for the given turn.
    pub fn write_tool_result(
        &mut self,
        turn: u64,
        tool_use_id: &str,
        name: &str,
        content: &str,
        is_error: bool,
    ) -> Result<(), std::io::Error> {
        let seq = self.next_seq();
        self.write_line(serde_json::json!({
            "v": SCHEMA_VERSION,
            "kind": "tool_result",
            "turn": turn,
            "seq": seq,
            "tool_use_id": tool_use_id,
            "name": name,
            "content": content,
            "is_error": is_error,
        }))
    }

    /// Record per-turn token usage.
    pub fn write_usage(
        &mut self,
        turn: u64,
        usage: &UsageSnapshot,
    ) -> Result<(), std::io::Error> {
        self.write_line(serde_json::json!({
            "v": SCHEMA_VERSION,
            "kind": "usage",
            "turn": turn,
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "cache_read_input_tokens": usage.cache_read_input_tokens,
            "cache_creation_input_tokens": usage.cache_creation_input_tokens,
        }))
    }

    /// Write the terminal `end` line. Idempotent: subsequent calls (and the
    /// [`Drop`] guard) are no-ops once an `end` line exists.
    pub fn write_end(
        &mut self,
        status: RunStatus,
        error: Option<&str>,
    ) -> Result<(), std::io::Error> {
        if self.ended {
            return Ok(());
        }
        self.ended = true;
        self.write_line(serde_json::json!({
            "v": SCHEMA_VERSION,
            "kind": "end",
            "turn": self.turn,
            "status": status.as_str(),
            "error": error,
        }))
    }

}

impl Drop for TrajectoryWriter {
    fn drop(&mut self) {
        // Best-effort terminal line for crash / panic / early-return paths that
        // never called `write_end`. We cannot know the real status here, so
        // mark it as an interrupted error.
        if !self.ended {
            if let Err(e) = self.write_end(RunStatus::Error, Some("interrupted: writer dropped before completion")) {
                tracing::warn!(path = %self.path, error = %e, "failed to write trajectory end line on drop");
            }
        }
    }
}

/// A process-wide handle to the active trajectory writer so a `SIGTERM` handler
/// can finalize it. `None` when no `--trajectory` was requested.
pub type SharedTrajectory = Arc<Mutex<Option<TrajectoryWriter>>>;

/// Finalize a shared trajectory with the given status, swallowing poisoning and
/// IO errors (best-effort, used from signal handlers and the normal exit path).
pub fn finalize(shared: &SharedTrajectory, status: RunStatus, error: Option<&str>) {
    if let Ok(mut guard) = shared.lock() {
        if let Some(writer) = guard.as_mut() {
            if let Err(e) = writer.write_end(status, error) {
                tracing::warn!(error = %e, "failed to finalize trajectory");
            }
        }
    }
}

/// RFC3339-ish UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`) without pulling in chrono.
///
/// Uses civil-date arithmetic (Howard Hinnant's algorithm) so month/day are
/// correct across leap years — adequate for trajectory metadata.
fn rfc3339_now() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // days since 1970-01-01 → civil (y, m, d), per Hinnant.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_lines(path: &std::path::Path) -> Vec<serde_json::Value> {
        let raw = std::fs::read_to_string(path).unwrap();
        raw.lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn test_meta_line_written_on_create() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traj.jsonl");
        let p = path.to_str().unwrap();
        {
            let mut w = TrajectoryWriter::create(p, "claude-sonnet-4-6", "sess-1").unwrap();
            w.write_end(RunStatus::Ok, None).unwrap();
        }
        let lines = read_lines(&path);
        assert_eq!(lines[0]["kind"], "meta");
        assert_eq!(lines[0]["v"], 1);
        assert_eq!(lines[0]["model"], "claude-sonnet-4-6");
        assert_eq!(lines[0]["session_id"], "sess-1");
        assert!(lines[0]["ts"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn test_full_sequence_and_monotonic_seq() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traj.jsonl");
        let p = path.to_str().unwrap();
        {
            let mut w = TrajectoryWriter::create(p, "m", "s").unwrap();
            let turn = w
                .write_assistant(
                    "thinking",
                    &[serde_json::json!({"id":"t1","name":"bash","arguments":{"command":"ls"}})],
                )
                .unwrap();
            w.write_tool_result(turn, "t1", "bash", "out", false).unwrap();
            w.write_usage(
                turn,
                &UsageSnapshot {
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_read_input_tokens: 2,
                    cache_creation_input_tokens: 1,
                },
            )
            .unwrap();
            w.write_assistant("done", &[]).unwrap();
            w.write_end(RunStatus::Ok, None).unwrap();
        }
        let lines = read_lines(&path);
        let kinds: Vec<&str> = lines.iter().map(|l| l["kind"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            vec!["meta", "assistant", "tool_result", "usage", "assistant", "end"]
        );
        // turn advances per assistant
        assert_eq!(lines[1]["turn"], 1);
        assert_eq!(lines[4]["turn"], 2);
        // tool_result references its turn
        assert_eq!(lines[2]["turn"], 1);
        assert_eq!(lines[2]["tool_use_id"], "t1");
        assert_eq!(lines[2]["is_error"], false);
        // usage fields present
        assert_eq!(lines[3]["input_tokens"], 10);
        assert_eq!(lines[3]["cache_read_input_tokens"], 2);
        assert_eq!(lines[3]["cache_creation_input_tokens"], 1);
        // seq monotonic across assistant + tool_result lines that carry it
        assert_eq!(lines[1]["seq"], 1);
        assert_eq!(lines[2]["seq"], 2);
        assert_eq!(lines[4]["seq"], 3);
        // end line
        assert_eq!(lines[5]["kind"], "end");
        assert_eq!(lines[5]["status"], "ok");
        assert!(lines[5]["error"].is_null());
        assert_eq!(lines[5]["turn"], 2);
    }

    #[test]
    fn test_drop_writes_end_even_without_explicit_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traj.jsonl");
        let p = path.to_str().unwrap();
        {
            let mut w = TrajectoryWriter::create(p, "m", "s").unwrap();
            w.write_assistant("partial", &[]).unwrap();
            // no write_end — simulate crash/early return
        }
        let lines = read_lines(&path);
        let last = lines.last().unwrap();
        assert_eq!(last["kind"], "end");
        assert_eq!(last["status"], "error");
        assert!(last["error"].as_str().unwrap().contains("interrupted"));
    }

    #[test]
    fn test_write_end_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traj.jsonl");
        let p = path.to_str().unwrap();
        {
            let mut w = TrajectoryWriter::create(p, "m", "s").unwrap();
            w.write_end(RunStatus::MaxTurns, None).unwrap();
            w.write_end(RunStatus::Ok, None).unwrap(); // ignored
        }
        let ends: Vec<_> = read_lines(&path)
            .into_iter()
            .filter(|l| l["kind"] == "end")
            .collect();
        assert_eq!(ends.len(), 1, "exactly one end line");
        assert_eq!(ends[0]["status"], "max_turns");
    }

    #[test]
    fn test_finalize_helper() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traj.jsonl");
        let p = path.to_str().unwrap();
        let shared: SharedTrajectory =
            Arc::new(Mutex::new(Some(TrajectoryWriter::create(p, "m", "s").unwrap())));
        finalize(&shared, RunStatus::Error, Some("sigterm"));
        // drop writer to flush/close
        drop(shared.lock().unwrap().take());
        let lines = read_lines(&path);
        let last = lines.last().unwrap();
        assert_eq!(last["kind"], "end");
        assert_eq!(last["status"], "error");
        assert_eq!(last["error"], "sigterm");
    }

    #[test]
    fn test_run_status_as_str() {
        assert_eq!(RunStatus::Ok.as_str(), "ok");
        assert_eq!(RunStatus::MaxTurns.as_str(), "max_turns");
        assert_eq!(RunStatus::Error.as_str(), "error");
    }

    #[test]
    fn test_rfc3339_now_shape() {
        let ts = rfc3339_now();
        // YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(ts.len(), 20, "got {ts}");
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }
}
