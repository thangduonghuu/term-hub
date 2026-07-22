use rusqlite::{params, OptionalExtension, Connection};
use std::path::Path;
use std::sync::Mutex;

use crate::session::SessionMeta;
use crate::usage::{AgentUsage, DayUsage, SessionUsage};

pub struct Db(pub Mutex<Connection>);

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                cwd TEXT NOT NULL,
                shell TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS usage_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT,
                agent TEXT NOT NULL,
                tokens_in INTEGER NOT NULL,
                tokens_out INTEGER NOT NULL,
                timestamp INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS usage_file_offsets (
                file_path TEXT PRIMARY KEY,
                byte_offset INTEGER NOT NULL
            );",
        )?;
        Ok(Db(Mutex::new(conn)))
    }

    pub fn insert_session(&self, meta: &SessionMeta) -> rusqlite::Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (id, name, cwd, shell, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![meta.id, meta.name, meta.cwd, meta.shell, meta.created_at],
        )?;
        Ok(())
    }

    pub fn rename_session(&self, id: &str, name: &str) -> rusqlite::Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET name = ?1 WHERE id = ?2",
            params![name, id],
        )?;
        Ok(())
    }

    pub fn delete_session(&self, id: &str) -> rusqlite::Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn get_session(&self, id: &str) -> rusqlite::Result<SessionMeta> {
        let conn = self.0.lock().unwrap();
        conn.query_row(
            "SELECT id, name, cwd, shell, created_at FROM sessions WHERE id = ?1",
            params![id],
            |row| {
                Ok(SessionMeta {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    cwd: row.get(2)?,
                    shell: row.get(3)?,
                    created_at: row.get(4)?,
                })
            },
        )
    }

    pub fn list_sessions(&self) -> rusqlite::Result<Vec<SessionMeta>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, name, cwd, shell, created_at FROM sessions ORDER BY created_at ASC")?;
        let rows = stmt.query_map([], |row| {
            Ok(SessionMeta {
                id: row.get(0)?,
                name: row.get(1)?,
                cwd: row.get(2)?,
                shell: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    pub fn get_file_offset(&self, path: &str) -> rusqlite::Result<u64> {
        let conn = self.0.lock().unwrap();
        let offset: Option<i64> = conn
            .query_row(
                "SELECT byte_offset FROM usage_file_offsets WHERE file_path = ?1",
                params![path],
                |row| row.get(0),
            )
            .optional()?;
        Ok(offset.unwrap_or(0) as u64)
    }

    pub fn set_file_offset(&self, path: &str, offset: u64) -> rusqlite::Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO usage_file_offsets (file_path, byte_offset) VALUES (?1, ?2)
             ON CONFLICT(file_path) DO UPDATE SET byte_offset = excluded.byte_offset",
            params![path, offset as i64],
        )?;
        Ok(())
    }

    /// Most-recently-created TermHub session whose cwd exactly matches, if any — the best
    /// available heuristic without shell integration to track a pane's live cwd.
    pub fn find_session_id_for_cwd(&self, cwd: &str) -> rusqlite::Result<Option<String>> {
        let conn = self.0.lock().unwrap();
        conn.query_row(
            "SELECT id FROM sessions WHERE cwd = ?1 ORDER BY created_at DESC LIMIT 1",
            params![cwd],
            |row| row.get(0),
        )
        .optional()
    }

    pub fn insert_usage_event(
        &self,
        session_id: Option<&str>,
        agent: &str,
        tokens_in: i64,
        tokens_out: i64,
        timestamp: i64,
    ) -> rusqlite::Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO usage_events (session_id, agent, tokens_in, tokens_out, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, agent, tokens_in, tokens_out, timestamp],
        )?;
        Ok(())
    }

    pub fn usage_per_session(&self) -> rusqlite::Result<Vec<SessionUsage>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT ue.session_id, COALESCE(s.name, 'Outside TermHub'),
                    SUM(ue.tokens_in), SUM(ue.tokens_out)
             FROM usage_events ue
             LEFT JOIN sessions s ON s.id = ue.session_id
             GROUP BY COALESCE(ue.session_id, '')
             ORDER BY SUM(ue.tokens_in) + SUM(ue.tokens_out) DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SessionUsage {
                session_id: row.get(0)?,
                session_name: row.get(1)?,
                tokens_in: row.get(2)?,
                tokens_out: row.get(3)?,
            })
        })?;
        rows.collect()
    }

    pub fn usage_per_agent(&self) -> rusqlite::Result<Vec<AgentUsage>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT agent, SUM(tokens_in), SUM(tokens_out) FROM usage_events
             GROUP BY agent ORDER BY SUM(tokens_in) + SUM(tokens_out) DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(AgentUsage {
                agent: row.get(0)?,
                tokens_in: row.get(1)?,
                tokens_out: row.get(2)?,
            })
        })?;
        rows.collect()
    }

    pub fn usage_per_day(&self) -> rusqlite::Result<Vec<DayUsage>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT strftime('%Y-%m-%d', timestamp, 'unixepoch') as day,
                    SUM(tokens_in), SUM(tokens_out)
             FROM usage_events GROUP BY day ORDER BY day DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(DayUsage {
                day: row.get(0)?,
                tokens_in: row.get(1)?,
                tokens_out: row.get(2)?,
            })
        })?;
        rows.collect()
    }

    pub fn usage_grand_total(&self) -> rusqlite::Result<(i64, i64)> {
        let conn = self.0.lock().unwrap();
        conn.query_row(
            "SELECT COALESCE(SUM(tokens_in), 0), COALESCE(SUM(tokens_out), 0) FROM usage_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }
}
