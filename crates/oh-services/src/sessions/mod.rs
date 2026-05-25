//! Persistent session storage backed by SQLite.
//!
//! Users can resume a prior conversation with `ohrs --resume <id>` or `ohrs -c`.

use async_trait::async_trait;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Session not found: {0}")]
    NotFound(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Task join error: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("Internal error: {0}")]
    Internal(String),
}

// ── Domain types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Archived,
    Deleted,
}

impl SessionStatus {
    fn as_str(&self) -> &'static str {
        match self {
            SessionStatus::Active => "active",
            SessionStatus::Archived => "archived",
            SessionStatus::Deleted => "deleted",
        }
    }

    fn from_str(s: &str) -> rusqlite::Result<Self> {
        match s {
            "active" => Ok(SessionStatus::Active),
            "archived" => Ok(SessionStatus::Archived),
            "deleted" => Ok(SessionStatus::Deleted),
            other => Err(rusqlite::Error::InvalidColumnType(
                7,
                format!("unknown SessionStatus '{other}'"),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    Tool,
    System,
}

impl MessageRole {
    fn as_str(&self) -> &'static str {
        match self {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
            MessageRole::System => "system",
        }
    }

    fn from_str(s: &str) -> rusqlite::Result<Self> {
        match s {
            "user" => Ok(MessageRole::User),
            "assistant" => Ok(MessageRole::Assistant),
            "tool" => Ok(MessageRole::Tool),
            "system" => Ok(MessageRole::System),
            other => Err(rusqlite::Error::InvalidColumnType(
                2,
                format!("unknown MessageRole '{other}'"),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub name: Option<String>,
    pub project_root: PathBuf,
    pub model: String,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    pub message_count: u32,
    pub status: SessionStatus,
    /// Id of the parent session for subagent runs; `None` for top-level
    /// sessions. Links a subagent trajectory back to its spawner.
    #[serde(default)]
    pub parent_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub id: String,
    pub session_id: String,
    pub role: MessageRole,
    pub content: serde_json::Value,
    pub seq: u64,
    pub created_at: SystemTime,
}

#[derive(Debug, Clone, Default)]
pub struct SessionFilter {
    pub project_root: Option<PathBuf>,
    pub status: Option<SessionStatus>,
    pub name_glob: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionPatch {
    pub name: Option<Option<String>>,
    pub status: Option<SessionStatus>,
    pub model: Option<String>,
}

// ── Backend trait ─────────────────────────────────────────────────────────────

#[async_trait]
pub trait SessionBackend: Send + Sync {
    async fn create_session(&self, rec: &SessionRecord) -> Result<(), SessionError>;
    async fn get_session(&self, id: &str) -> Result<Option<SessionRecord>, SessionError>;
    async fn list_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<SessionRecord>, SessionError>;
    async fn update_session(&self, id: &str, patch: SessionPatch) -> Result<(), SessionError>;
    async fn delete_session(&self, id: &str) -> Result<(), SessionError>;
    async fn append_message(&self, msg: &SessionMessage) -> Result<(), SessionError>;
    async fn list_messages(
        &self,
        session_id: &str,
        since_seq: Option<u64>,
    ) -> Result<Vec<SessionMessage>, SessionError>;
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn system_time_to_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

fn secs_to_system_time(secs: i64) -> SystemTime {
    UNIX_EPOCH + std::time::Duration::from_secs(secs.max(0) as u64)
}

// ── SQLite backend ────────────────────────────────────────────────────────────

const SCHEMA: &str = "
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS sessions(
    id                TEXT PRIMARY KEY,
    name              TEXT,
    project_root      TEXT NOT NULL,
    model             TEXT NOT NULL,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    message_count     INTEGER NOT NULL DEFAULT 0,
    status            TEXT NOT NULL DEFAULT 'active'
                      CHECK(status IN ('active','archived','deleted')),
    parent_session_id TEXT
);

CREATE TABLE IF NOT EXISTS messages(
    id         TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    role       TEXT NOT NULL
               CHECK(role IN ('user','assistant','tool','system')),
    content    TEXT NOT NULL,
    seq        INTEGER NOT NULL CHECK(seq >= 0),
    created_at INTEGER NOT NULL,
    FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE,
    UNIQUE(session_id, seq)
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);
";

/// SQLite-backed session store.
pub struct SqliteBackend {
    db_path: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl SqliteBackend {
    /// Open (or create) a SQLite database at `path` and initialise the schema.
    pub async fn new(path: &Path) -> Result<Self, SessionError> {
        let path_owned = path.to_path_buf();
        let conn = tokio::task::spawn_blocking(move || -> Result<Connection, SessionError> {
            if let Some(parent) = path_owned.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let conn = Connection::open(&path_owned)?;
            conn.execute_batch(SCHEMA)?;
            // Additive migration for DBs created before `parent_session_id`
            // existed. `ALTER TABLE ADD COLUMN` errors if the column is already
            // present, so ignore that specific failure.
            if let Err(e) =
                conn.execute("ALTER TABLE sessions ADD COLUMN parent_session_id TEXT", [])
            {
                if !e.to_string().contains("duplicate column name") {
                    return Err(e.into());
                }
            }
            Ok(conn)
        })
        .await??;

        Ok(Self {
            db_path: path.to_path_buf(),
            conn: Arc::new(Mutex::new(conn)),
        })
    }
}

// Convenience: clone the Arc so closures can be moved into spawn_blocking.
impl SqliteBackend {
    fn conn(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    let project_root: String = row.get(2)?;
    let created_at: i64 = row.get(4)?;
    let updated_at: i64 = row.get(5)?;
    let message_count_raw: i64 = row.get(6)?;
    let status_str: String = row.get(7)?;
    Ok(SessionRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        project_root: PathBuf::from(project_root),
        model: row.get(3)?,
        created_at: secs_to_system_time(created_at),
        updated_at: secs_to_system_time(updated_at),
        message_count: message_count_raw.max(0) as u32,
        status: SessionStatus::from_str(&status_str)?,
        parent_session_id: row.get(8)?,
    })
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionMessage> {
    let role_str: String = row.get(2)?;
    let content_str: String = row.get(3)?;
    let seq_raw: i64 = row.get(4)?;
    let created_at: i64 = row.get(5)?;
    Ok(SessionMessage {
        id: row.get(0)?,
        session_id: row.get(1)?,
        role: MessageRole::from_str(&role_str)?,
        content: serde_json::from_str(&content_str).map_err(|e| {
            rusqlite::Error::InvalidColumnType(
                3,
                format!("invalid JSON content: {e}"),
                rusqlite::types::Type::Text,
            )
        })?,
        seq: seq_raw.max(0) as u64,
        created_at: secs_to_system_time(created_at),
    })
}

#[async_trait]
impl SessionBackend for SqliteBackend {
    async fn create_session(&self, rec: &SessionRecord) -> Result<(), SessionError> {
        let conn = self.conn();
        let id = rec.id.clone();
        let name = rec.name.clone();
        let project_root = rec.project_root.to_string_lossy().to_string();
        let model = rec.model.clone();
        let created_at = system_time_to_secs(rec.created_at);
        let updated_at = system_time_to_secs(rec.updated_at);
        let message_count = rec.message_count as i64;
        let status = rec.status.as_str().to_string();
        let parent_session_id = rec.parent_session_id.clone();

        tokio::task::spawn_blocking(move || -> Result<(), SessionError> {
            let conn = conn.lock().map_err(|e| SessionError::Internal(e.to_string()))?;
            conn.execute(
                "INSERT INTO sessions(id, name, project_root, model, created_at, updated_at, message_count, status, parent_session_id)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![id, name, project_root, model, created_at, updated_at, message_count, status, parent_session_id],
            )?;
            Ok(())
        })
        .await??;
        Ok(())
    }

    async fn get_session(&self, id: &str) -> Result<Option<SessionRecord>, SessionError> {
        let conn = self.conn();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<SessionRecord>, SessionError> {
            let conn = conn.lock().map_err(|e| SessionError::Internal(e.to_string()))?;
            let mut stmt = conn.prepare(
                "SELECT id, name, project_root, model, created_at, updated_at, message_count, status, parent_session_id
                 FROM sessions WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id])?;
            if let Some(row) = rows.next()? {
                Ok(Some(row_to_record(row)?))
            } else {
                Ok(None)
            }
        })
        .await?
    }

    async fn list_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        let conn = self.conn();
        tokio::task::spawn_blocking(move || -> Result<Vec<SessionRecord>, SessionError> {
            let conn = conn.lock().map_err(|e| SessionError::Internal(e.to_string()))?;

            // Build query dynamically
            let mut conditions: Vec<String> = Vec::new();
            if filter.project_root.is_some() {
                conditions.push("project_root = ?1".to_string());
            }
            if filter.status.is_some() {
                conditions.push(format!("status = ?{}", conditions.len() + 1));
            }
            if filter.name_glob.is_some() {
                conditions.push(format!("name GLOB ?{}", conditions.len() + 1));
            }

            let where_clause = if conditions.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", conditions.join(" AND "))
            };

            let limit_clause = filter
                .limit
                .map(|l| format!("LIMIT {}", l))
                .unwrap_or_default();

            let sql = format!(
                "SELECT id, name, project_root, model, created_at, updated_at, message_count, status, parent_session_id
                 FROM sessions {where_clause} ORDER BY updated_at DESC {limit_clause}"
            );

            let mut stmt = conn.prepare(&sql)?;

            // Bind params in order
            let mut param_idx = 1usize;
            let mut bound_values: Vec<String> = Vec::new();
            if let Some(ref pr) = filter.project_root {
                bound_values.push(pr.to_string_lossy().to_string());
                param_idx += 1;
            }
            if let Some(ref s) = filter.status {
                bound_values.push(s.as_str().to_string());
                param_idx += 1;
            }
            if let Some(ref g) = filter.name_glob {
                bound_values.push(g.clone());
                param_idx += 1;
            }
            let _ = param_idx; // suppress unused warning

            let rows = stmt.query_map(
                rusqlite::params_from_iter(bound_values.iter()),
                row_to_record,
            )?;

            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok(results)
        })
        .await?
    }

    async fn update_session(&self, id: &str, patch: SessionPatch) -> Result<(), SessionError> {
        let conn = self.conn();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), SessionError> {
            let conn = conn
                .lock()
                .map_err(|e| SessionError::Internal(e.to_string()))?;

            if let Some(name_opt) = patch.name {
                conn.execute(
                    "UPDATE sessions SET name = ?1, updated_at = ?2 WHERE id = ?3",
                    params![name_opt, system_time_to_secs(SystemTime::now()), id],
                )?;
            }
            if let Some(status) = patch.status {
                conn.execute(
                    "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
                    params![status.as_str(), system_time_to_secs(SystemTime::now()), id],
                )?;
            }
            if let Some(model) = patch.model {
                conn.execute(
                    "UPDATE sessions SET model = ?1, updated_at = ?2 WHERE id = ?3",
                    params![model, system_time_to_secs(SystemTime::now()), id],
                )?;
            }
            Ok(())
        })
        .await??;
        Ok(())
    }

    async fn delete_session(&self, id: &str) -> Result<(), SessionError> {
        let conn = self.conn();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), SessionError> {
            let conn = conn
                .lock()
                .map_err(|e| SessionError::Internal(e.to_string()))?;
            // Enable foreign keys for this connection to ensure CASCADE works
            conn.execute_batch("PRAGMA foreign_keys=ON;")?;
            conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
            Ok(())
        })
        .await??;
        Ok(())
    }

    async fn append_message(&self, msg: &SessionMessage) -> Result<(), SessionError> {
        let conn = self.conn();
        let id = msg.id.clone();
        let session_id = msg.session_id.clone();
        let role = msg.role.as_str().to_string();
        let content = serde_json::to_string(&msg.content)?;
        let seq = msg.seq as i64;
        let created_at = system_time_to_secs(msg.created_at);

        tokio::task::spawn_blocking(move || -> Result<(), SessionError> {
            let conn = conn.lock().map_err(|e| SessionError::Internal(e.to_string()))?;
            let now = system_time_to_secs(SystemTime::now());

            // Insert message and update session in one transaction
            conn.execute_batch("BEGIN;")?;
            let result = (|| -> Result<(), SessionError> {
                conn.execute(
                    "INSERT INTO messages(id, session_id, role, content, seq, created_at)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, session_id, role, content, seq, created_at],
                )?;
                conn.execute(
                    "UPDATE sessions SET message_count = message_count + 1, updated_at = ?1 WHERE id = ?2",
                    params![now, session_id],
                )?;
                Ok(())
            })();
            if result.is_ok() {
                conn.execute_batch("COMMIT;")?;
            } else {
                conn.execute_batch("ROLLBACK;")?;
            }
            result
        })
        .await??;
        Ok(())
    }

    async fn list_messages(
        &self,
        session_id: &str,
        since_seq: Option<u64>,
    ) -> Result<Vec<SessionMessage>, SessionError> {
        let conn = self.conn();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<SessionMessage>, SessionError> {
            let conn = conn
                .lock()
                .map_err(|e| SessionError::Internal(e.to_string()))?;

            let (sql, since) = if let Some(seq) = since_seq {
                (
                    "SELECT id, session_id, role, content, seq, created_at
                     FROM messages WHERE session_id = ?1 AND seq > ?2 ORDER BY seq ASC",
                    Some(seq as i64),
                )
            } else {
                (
                    "SELECT id, session_id, role, content, seq, created_at
                     FROM messages WHERE session_id = ?1 ORDER BY seq ASC",
                    None,
                )
            };

            let rows: Vec<SessionMessage> = if let Some(s) = since {
                let mut stmt = conn.prepare(sql)?;
                let x = stmt
                    .query_map(params![session_id, s], row_to_message)?
                    .collect::<Result<Vec<_>, _>>()?;
                x
            } else {
                let mut stmt = conn.prepare(sql)?;
                let x = stmt
                    .query_map(params![session_id], row_to_message)?
                    .collect::<Result<Vec<_>, _>>()?;
                x
            };

            Ok(rows)
        })
        .await?
    }
}

// ── SessionStore ──────────────────────────────────────────────────────────────

/// High-level session store wrapping a pluggable backend.
pub struct SessionStore {
    backend: Box<dyn SessionBackend>,
}

impl SessionStore {
    /// Create a store with a custom backend (useful for testing).
    pub fn with_backend(backend: Box<dyn SessionBackend>) -> Self {
        Self { backend }
    }

    /// Create a store backed by SQLite at `~/.config/ohrs/sessions.db`.
    pub async fn with_default_backend() -> Result<Self, SessionError> {
        let db_path = oh_config::get_sessions_dir().join("sessions.db");
        let backend = SqliteBackend::new(&db_path).await?;
        Ok(Self {
            backend: Box::new(backend),
        })
    }

    pub async fn create_session(&self, rec: &SessionRecord) -> Result<(), SessionError> {
        self.backend.create_session(rec).await
    }

    pub async fn get_session(&self, id: &str) -> Result<Option<SessionRecord>, SessionError> {
        self.backend.get_session(id).await
    }

    pub async fn list_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        self.backend.list_sessions(filter).await
    }

    pub async fn update_session(&self, id: &str, patch: SessionPatch) -> Result<(), SessionError> {
        self.backend.update_session(id, patch).await
    }

    pub async fn delete_session(&self, id: &str) -> Result<(), SessionError> {
        self.backend.delete_session(id).await
    }

    pub async fn append_message(&self, msg: &SessionMessage) -> Result<(), SessionError> {
        self.backend.append_message(msg).await
    }

    pub async fn list_messages(
        &self,
        session_id: &str,
        since_seq: Option<u64>,
    ) -> Result<Vec<SessionMessage>, SessionError> {
        self.backend.list_messages(session_id, since_seq).await
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_record(id: &str, project_root: &Path) -> SessionRecord {
        SessionRecord {
            id: id.to_string(),
            name: None,
            project_root: project_root.to_path_buf(),
            model: "claude-3-opus".to_string(),
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            message_count: 0,
            status: SessionStatus::Active,
            parent_session_id: None,
        }
    }

    fn make_message(id: &str, session_id: &str, seq: u64) -> SessionMessage {
        SessionMessage {
            id: id.to_string(),
            session_id: session_id.to_string(),
            role: MessageRole::User,
            content: serde_json::json!({"text": format!("message {seq}")}),
            seq,
            created_at: SystemTime::now(),
        }
    }

    async fn make_backend(dir: &tempfile::TempDir) -> SqliteBackend {
        SqliteBackend::new(&dir.path().join("test.db"))
            .await
            .unwrap()
    }

    // ── create + retrieve session ────────────────────────────────────────────

    #[tokio::test]
    async fn test_create_and_get_session() {
        let dir = tempfile::tempdir().unwrap();
        let backend = make_backend(&dir).await;
        let rec = make_record("sess-001", dir.path());
        backend.create_session(&rec).await.unwrap();
        let got = backend.get_session("sess-001").await.unwrap();
        assert!(got.is_some());
        let got = got.unwrap();
        assert_eq!(got.id, "sess-001");
        assert_eq!(got.model, "claude-3-opus");
        assert_eq!(got.status, SessionStatus::Active);
    }

    #[tokio::test]
    async fn test_get_nonexistent_session_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let backend = make_backend(&dir).await;
        let got = backend.get_session("does-not-exist").await.unwrap();
        assert!(got.is_none());
    }

    // ── list with filter ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_list_filter_by_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let backend = make_backend(&dir).await;

        let proj_a = dir.path().join("proj_a");
        let proj_b = dir.path().join("proj_b");

        let mut r1 = make_record("sess-a1", &proj_a);
        r1.id = "sess-a1".to_string();
        let mut r2 = make_record("sess-a2", &proj_a);
        r2.id = "sess-a2".to_string();
        let mut r3 = make_record("sess-b1", &proj_b);
        r3.id = "sess-b1".to_string();

        backend.create_session(&r1).await.unwrap();
        backend.create_session(&r2).await.unwrap();
        backend.create_session(&r3).await.unwrap();

        let filter = SessionFilter {
            project_root: Some(proj_a.clone()),
            ..Default::default()
        };
        let results = backend.list_sessions(filter).await.unwrap();
        assert_eq!(results.len(), 2);
        for r in &results {
            assert_eq!(r.project_root, proj_a);
        }
    }

    #[tokio::test]
    async fn test_list_filter_by_status() {
        let dir = tempfile::tempdir().unwrap();
        let backend = make_backend(&dir).await;
        let proj = dir.path().to_path_buf();

        let mut r1 = make_record("sess-1", &proj);
        r1.id = "sess-1".to_string();
        r1.status = SessionStatus::Active;
        let mut r2 = make_record("sess-2", &proj);
        r2.id = "sess-2".to_string();
        r2.status = SessionStatus::Archived;

        backend.create_session(&r1).await.unwrap();
        backend.create_session(&r2).await.unwrap();

        let filter = SessionFilter {
            status: Some(SessionStatus::Archived),
            ..Default::default()
        };
        let results = backend.list_sessions(filter).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, SessionStatus::Archived);
    }

    // ── append messages + list in seq order ─────────────────────────────────

    #[tokio::test]
    async fn test_append_and_list_messages_in_seq_order() {
        let dir = tempfile::tempdir().unwrap();
        let backend = make_backend(&dir).await;
        let rec = make_record("sess-msg", dir.path());
        backend.create_session(&rec).await.unwrap();

        // Append out of order intentionally
        backend
            .append_message(&make_message("m3", "sess-msg", 3))
            .await
            .unwrap();
        backend
            .append_message(&make_message("m1", "sess-msg", 1))
            .await
            .unwrap();
        backend
            .append_message(&make_message("m2", "sess-msg", 2))
            .await
            .unwrap();

        let msgs = backend.list_messages("sess-msg", None).await.unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].seq, 1);
        assert_eq!(msgs[1].seq, 2);
        assert_eq!(msgs[2].seq, 3);
    }

    // ── since_seq filter ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_list_messages_since_seq() {
        let dir = tempfile::tempdir().unwrap();
        let backend = make_backend(&dir).await;
        let rec = make_record("sess-since", dir.path());
        backend.create_session(&rec).await.unwrap();

        for i in 1u64..=5 {
            backend
                .append_message(&make_message(&format!("m{i}"), "sess-since", i))
                .await
                .unwrap();
        }

        let msgs = backend.list_messages("sess-since", Some(2)).await.unwrap();
        assert_eq!(msgs.len(), 3); // seqs 3, 4, 5
        assert_eq!(msgs[0].seq, 3);
        assert_eq!(msgs[2].seq, 5);
    }

    // ── update session name ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_update_session_name() {
        let dir = tempfile::tempdir().unwrap();
        let backend = make_backend(&dir).await;
        let rec = make_record("sess-upd", dir.path());
        backend.create_session(&rec).await.unwrap();

        let patch = SessionPatch {
            name: Some(Some("my-session".to_string())),
            ..Default::default()
        };
        backend.update_session("sess-upd", patch).await.unwrap();

        let got = backend.get_session("sess-upd").await.unwrap().unwrap();
        assert_eq!(got.name, Some("my-session".to_string()));
    }

    // ── delete cascades to messages ──────────────────────────────────────────

    #[tokio::test]
    async fn test_delete_session_cascades_messages() {
        let dir = tempfile::tempdir().unwrap();
        let backend = make_backend(&dir).await;
        let rec = make_record("sess-del", dir.path());
        backend.create_session(&rec).await.unwrap();

        backend
            .append_message(&make_message("dm1", "sess-del", 1))
            .await
            .unwrap();
        backend
            .append_message(&make_message("dm2", "sess-del", 2))
            .await
            .unwrap();

        // Verify messages exist
        let before = backend.list_messages("sess-del", None).await.unwrap();
        assert_eq!(before.len(), 2);

        // Delete session
        backend.delete_session("sess-del").await.unwrap();

        // Session is gone
        let got = backend.get_session("sess-del").await.unwrap();
        assert!(got.is_none());

        // Messages are gone too
        let after = backend.list_messages("sess-del", None).await.unwrap();
        assert!(after.is_empty());
    }

    // ── two sessions in same project don't interfere ─────────────────────────

    #[tokio::test]
    async fn test_two_sessions_same_project_no_interference() {
        let dir = tempfile::tempdir().unwrap();
        let backend = make_backend(&dir).await;
        let proj = dir.path().to_path_buf();

        let r1 = make_record("sess-x1", &proj);
        let r2 = make_record("sess-x2", &proj);
        backend.create_session(&r1).await.unwrap();
        backend.create_session(&r2).await.unwrap();

        // Append messages to each
        backend
            .append_message(&make_message("mx1-1", "sess-x1", 1))
            .await
            .unwrap();
        backend
            .append_message(&make_message("mx1-2", "sess-x1", 2))
            .await
            .unwrap();
        backend
            .append_message(&make_message("mx2-1", "sess-x2", 1))
            .await
            .unwrap();

        let msgs1 = backend.list_messages("sess-x1", None).await.unwrap();
        let msgs2 = backend.list_messages("sess-x2", None).await.unwrap();

        assert_eq!(msgs1.len(), 2);
        assert_eq!(msgs2.len(), 1);

        // message_count on each session
        let s1 = backend.get_session("sess-x1").await.unwrap().unwrap();
        let s2 = backend.get_session("sess-x2").await.unwrap().unwrap();
        assert_eq!(s1.message_count, 2);
        assert_eq!(s2.message_count, 1);
    }

    // ── append_message increments message_count ──────────────────────────────

    #[tokio::test]
    async fn test_append_increments_message_count() {
        let dir = tempfile::tempdir().unwrap();
        let backend = make_backend(&dir).await;
        let rec = make_record("sess-cnt", dir.path());
        backend.create_session(&rec).await.unwrap();

        for i in 1u64..=4 {
            backend
                .append_message(&make_message(&format!("cnt-m{i}"), "sess-cnt", i))
                .await
                .unwrap();
        }

        let s = backend.get_session("sess-cnt").await.unwrap().unwrap();
        assert_eq!(s.message_count, 4);
    }

    // ── SessionStore wrapper ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_session_store_with_backend() {
        let dir = tempfile::tempdir().unwrap();
        let backend = make_backend(&dir).await;
        let store = SessionStore::with_backend(Box::new(backend));

        let rec = make_record("store-sess", dir.path());
        store.create_session(&rec).await.unwrap();

        let got = store.get_session("store-sess").await.unwrap();
        assert!(got.is_some());
    }

    // ── parent_session_id round-trips (parent + child) ───────────────────────

    #[tokio::test]
    async fn test_parent_child_session_link_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let backend = make_backend(&dir).await;

        // Parent session has no parent.
        let parent = make_record("parent-sess", dir.path());
        backend.create_session(&parent).await.unwrap();

        // Child session links to the parent.
        let mut child = make_record("child-sess", dir.path());
        child.parent_session_id = Some("parent-sess".to_string());
        backend.create_session(&child).await.unwrap();

        let got_parent = backend.get_session("parent-sess").await.unwrap().unwrap();
        assert_eq!(got_parent.parent_session_id, None);

        let got_child = backend.get_session("child-sess").await.unwrap().unwrap();
        assert_eq!(got_child.parent_session_id, Some("parent-sess".to_string()));

        // The link also survives list_sessions().
        let listed = backend
            .list_sessions(SessionFilter {
                project_root: Some(dir.path().to_path_buf()),
                ..Default::default()
            })
            .await
            .unwrap();
        let child_listed = listed
            .iter()
            .find(|s| s.id == "child-sess")
            .expect("child should be listed");
        assert_eq!(
            child_listed.parent_session_id,
            Some("parent-sess".to_string())
        );
    }

    // ── list_sessions with limit ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_list_sessions_limit() {
        let dir = tempfile::tempdir().unwrap();
        let backend = make_backend(&dir).await;
        let proj = dir.path().to_path_buf();

        for i in 0..10u32 {
            let mut r = make_record(&format!("lim-{i}"), &proj);
            r.id = format!("lim-{i}");
            // stagger updated_at slightly so ordering is deterministic
            r.updated_at = UNIX_EPOCH + Duration::from_secs(i as u64 * 10);
            backend.create_session(&r).await.unwrap();
        }

        let filter = SessionFilter {
            limit: Some(3),
            ..Default::default()
        };
        let results = backend.list_sessions(filter).await.unwrap();
        assert_eq!(results.len(), 3);
    }
}
