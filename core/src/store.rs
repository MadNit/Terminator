//! Saved connection profiles (rusqlite).
//!
//! Secrets never live here -- only a reference to a keychain entry. See
//! `secrets.rs`.

use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub group: Option<String>,
    /// Serialized TransportSpec.
    pub spec: serde_json::Value,
}

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS profiles (
                id       TEXT PRIMARY KEY,
                name     TEXT NOT NULL,
                grp      TEXT,
                spec     TEXT NOT NULL,
                created  INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );

            -- Per-command history, populated from OSC 133 markers.
            CREATE TABLE IF NOT EXISTS commands (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id  TEXT NOT NULL,
                command     TEXT NOT NULL,
                exit_code   INTEGER,
                duration_ms INTEGER,
                ran_at      INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );
            CREATE INDEX IF NOT EXISTS idx_commands_session ON commands(session_id);

            -- Full-text search across everything ever logged.
            CREATE VIRTUAL TABLE IF NOT EXISTS command_fts
                USING fts5(command, content='commands', content_rowid='id');
            "#,
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn save_profile(
        &self,
        name: &str,
        group: Option<&str>,
        spec: &serde_json::Value,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO profiles (id, name, grp, spec) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, group, spec.to_string()],
        )?;
        Ok(id)
    }

    /// Rewrites an existing profile in place, keeping its id so anything
    /// referring to it (open tabs, the sidebar selection) stays valid.
    ///
    /// Returns an error rather than silently doing nothing when the id is
    /// unknown, so a stale UI can't quietly discard the user's edit.
    pub fn update_profile(
        &self,
        id: &str,
        name: &str,
        group: Option<&str>,
        spec: &serde_json::Value,
    ) -> Result<()> {
        let c = self.conn.lock().unwrap();
        let n = c.execute(
            "UPDATE profiles SET name = ?2, grp = ?3, spec = ?4 WHERE id = ?1",
            params![id, name, group, spec.to_string()],
        )?;
        if n == 0 {
            anyhow::bail!("no profile with id {id}");
        }
        Ok(())
    }

    pub fn list_profiles(&self) -> Result<Vec<Profile>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare("SELECT id, name, grp, spec FROM profiles ORDER BY name")?;
        let rows = stmt
            .query_map([], |r| {
                let spec: String = r.get(3)?;
                Ok(Profile {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    group: r.get(2)?,
                    spec: serde_json::from_str(&spec).unwrap_or(serde_json::Value::Null),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete_profile(&self, id: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute("DELETE FROM profiles WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn record_command(
        &self,
        session_id: &str,
        command: &str,
        exit_code: Option<i32>,
        duration_ms: u64,
    ) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO commands (session_id, command, exit_code, duration_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_id, command, exit_code, duration_ms as i64],
        )?;
        let rowid = c.last_insert_rowid();
        c.execute(
            "INSERT INTO command_fts (rowid, command) VALUES (?1, ?2)",
            params![rowid, command],
        )?;
        Ok(())
    }

    /// Every command recorded for one session, oldest first.
    pub fn session_commands(&self, session_id: &str) -> Result<Vec<(String, Option<i32>, u64)>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT command, exit_code, duration_ms FROM commands
             WHERE session_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![session_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u64))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Search command history across every session ever recorded.
    pub fn search_commands(&self, query: &str, limit: u32) -> Result<Vec<(String, Option<i32>)>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT c.command, c.exit_code FROM command_fts f
             JOIN commands c ON c.id = f.rowid
             WHERE command_fts MATCH ?1
             ORDER BY c.ran_at DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![query, limit], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        (s, dir)
    }

    #[test]
    fn update_profile_edits_in_place() {
        let (s, _d) = store();
        let id = s
            .save_profile("old", None, &json!({"kind": "ssh", "host": "a"}))
            .unwrap();

        s.update_profile(&id, "new", None, &json!({"kind": "ssh", "host": "b"}))
            .unwrap();

        let ps = s.list_profiles().unwrap();
        assert_eq!(ps.len(), 1, "editing must not create a second profile");
        assert_eq!(ps[0].id, id, "id must survive an edit");
        assert_eq!(ps[0].name, "new");
        assert_eq!(ps[0].spec["host"], "b");
    }

    #[test]
    fn update_profile_rejects_unknown_id() {
        let (s, _d) = store();
        assert!(s.update_profile("nope", "n", None, &json!({})).is_err());
    }
}
