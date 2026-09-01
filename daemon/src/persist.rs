//! SQLite-backed session metadata store for the daemon.
//!
//! We persist `{id, spec, opened_at_ms, closed_at_ms}` so the
//! reattach prompt can show "previously open" sessions even
//! after the daemon restarts. Scrollback itself stays in
//! memory (in `OutputRingBuffer`) -- persisting that is a
//! much bigger problem and not in scope for v1.
//!
//! Schema is intentionally minimal: one row per session, with
//! a nullable `closed_at_ms` distinguishing "still alive" from
//! "user closed this". The daemon's `core::SessionManager` is
//! the source of truth for the "still alive" flag at runtime;
//! this table is the source of truth across restarts.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use tokio::sync::Mutex;
use tracing::warn;

use terminator_core::transport::TransportSpec;

/// One row in the `sessions` table, shaped for the frontend.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PersistedSession {
    pub id: String,
    pub spec: TransportSpec,
    pub opened_at_ms: i64,
    /// `None` means the session is still alive (in the core's
    /// map). Some means the user (or the core) closed it.
    pub closed_at_ms: Option<i64>,
}

pub struct SessionStore {
    conn: Mutex<Connection>,
}

impl SessionStore {
    /// Open or create the SQLite file at `path`. Creates the
    /// schema if needed. The store is process-local (one
    /// Connection); all access goes through the Mutex.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create parent dir {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open sqlite at {}", path.display()))?;
        // WAL gives us concurrent reads during a write --
        // important because the reattach prompt hits the
        // store on every app start while the user is
        // actively creating new sessions.
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("enable WAL")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id              TEXT PRIMARY KEY,
                spec            TEXT NOT NULL,
                opened_at_ms    INTEGER NOT NULL,
                closed_at_ms    INTEGER
            );
            CREATE INDEX IF NOT EXISTS sessions_opened_idx
                ON sessions(opened_at_ms DESC);
            "#,
        )
        .context("create schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Record a newly-opened session. Idempotent: if the id is
    /// already present, we leave the existing row alone
    /// (preserves the original opened_at_ms).
    pub async fn record_open(
        &self,
        id: uuid::Uuid,
        spec: &TransportSpec,
        opened_at_ms: i64,
    ) -> Result<()> {
        let spec_json = serde_json::to_string(spec).context("serialize spec")?;
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO sessions (id, spec, opened_at_ms, closed_at_ms) \
             VALUES (?1, ?2, ?3, NULL)",
            params![id.to_string(), spec_json, opened_at_ms],
        )
        .context("insert session")?;
        Ok(())
    }

    /// Mark a session as closed at `closed_at_ms`. If the id is
    /// unknown, this is a no-op (the session was already
    /// forgotten, which is fine).
    pub async fn record_close(&self, id: uuid::Uuid, closed_at_ms: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        let updated = conn
            .execute(
                "UPDATE sessions SET closed_at_ms = ?2 WHERE id = ?1 AND closed_at_ms IS NULL",
                params![id.to_string(), closed_at_ms],
            )
            .context("update closed_at_ms")?;
        if updated == 0 {
            // Either the id was never recorded (e.g. open
            // happened before this build was deployed) or it
            // was already closed. Neither is an error.
            warn!(%id, "close: no live row to update");
        }
        Ok(())
    }

    /// All known sessions, newest first. Callers (currently
    /// just `GET /sessions`) merge in the `alive` flag from
    /// the live core map.
    pub async fn list_all(&self) -> Result<Vec<PersistedSession>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, spec, opened_at_ms, closed_at_ms \
                 FROM sessions ORDER BY opened_at_ms DESC",
            )
            .context("prepare list")?;
        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let spec_json: String = row.get(1)?;
                let opened_at_ms: i64 = row.get(2)?;
                let closed_at_ms: Option<i64> = row.get(3)?;
                let spec: TransportSpec = serde_json::from_str(&spec_json)
                    .map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;
                Ok(PersistedSession {
                    id,
                    spec,
                    opened_at_ms,
                    closed_at_ms,
                })
            })
            .context("query list")?
            .collect::<Result<Vec<_>, _>>()
            .context("collect list")?;
        Ok(rows)
    }
}
