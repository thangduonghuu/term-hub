use rusqlite::{params, OptionalExtension, Connection};
use std::path::Path;
use std::sync::Mutex;

use crate::message::{LogEntry, Message};
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
            );
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS recent_folders (
                path TEXT PRIMARY KEY,
                last_opened_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                from_session TEXT,
                to_session TEXT NOT NULL,
                body TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                read_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_messages_inbox
                ON messages (to_session, read_at, id);",
        )?;
        // Backfill from whatever sessions already exist (e.g. every session predating the
        // `recent_folders` table, or a session restored at startup — `App::new`'s reconnect
        // path reads `sessions` directly and never calls `create_session`, so it never touches
        // this table on its own). Otherwise the Open Recent picker looks empty on first use
        // even with sessions already open, and its only actionable row is "Browse for
        // folder…" — which just opens a native picker, easily mistaken for the feature being
        // broken. Idempotent upsert, safe to run on every open.
        conn.execute(
            "INSERT INTO recent_folders (path, last_opened_at)
             SELECT cwd, MAX(created_at) FROM sessions GROUP BY cwd
             ON CONFLICT(path) DO UPDATE SET
                last_opened_at = MAX(last_opened_at, excluded.last_opened_at)",
            [],
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
        // Drop any inter-session messages addressed to (or sent by) a session that no longer
        // exists — an unread message to a closed session is undeliverable, and keeping it would
        // leak a stale unread count for an id nothing can select anymore.
        conn.execute(
            "DELETE FROM messages WHERE to_session = ?1 OR from_session = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Discards every saved session in one shot — used when the user declines the "reopen
    /// previous sessions?" prompt on launch (`run()`), so there's nothing stale left to ask
    /// about again next time. Doesn't touch `recent_folders`: those folders should still be
    /// reachable from the Open Recent picker even if their sessions were declined.
    pub fn clear_sessions(&self) -> rusqlite::Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute("DELETE FROM sessions", [])?;
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

    /// Caps the "Open Recent" MRU list at this many entries — old ones fall off as new folders
    /// get opened, same idea as VSCode's own recent-folders list.
    const RECENT_FOLDERS_LIMIT: i64 = 30;

    /// Records `path` as just-opened for the "Open Recent" picker (`create_session`'s caller).
    /// Independent of the `sessions` table — unlike a session row, this survives `close_session`,
    /// since the whole point is remembering folders you've opened even after you're done with
    /// them. Re-opening an already-listed folder just bumps its timestamp (upsert), not a dupe.
    pub fn touch_recent_folder(&self, path: &str, timestamp: i64) -> rusqlite::Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO recent_folders (path, last_opened_at) VALUES (?1, ?2)
             ON CONFLICT(path) DO UPDATE SET last_opened_at = excluded.last_opened_at",
            params![path, timestamp],
        )?;
        conn.execute(
            "DELETE FROM recent_folders WHERE path NOT IN (
                SELECT path FROM recent_folders ORDER BY last_opened_at DESC LIMIT ?1
            )",
            params![Self::RECENT_FOLDERS_LIMIT],
        )?;
        Ok(())
    }

    /// Most-recently-opened folders first, for the "Open Recent" picker.
    pub fn list_recent_folders(&self) -> rusqlite::Result<Vec<String>> {
        let conn = self.0.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT path FROM recent_folders ORDER BY last_opened_at DESC")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect()
    }

    /// Removes one entry from the "Open Recent" list (the picker's per-row X) — never touches
    /// the `sessions` table, so it has no effect on any session that happens to still be open in
    /// that folder.
    pub fn remove_recent_folder(&self, path: &str) -> rusqlite::Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute("DELETE FROM recent_folders WHERE path = ?1", params![path])?;
        Ok(())
    }

    /// Records one inter-session message (see `docs/intersession-messaging-plan.md`). `from` is
    /// the sender's session id, or `None` when sent from a plain shell outside any session.
    /// Returns the new row id.
    pub fn insert_message(
        &self,
        from: Option<&str>,
        to: &str,
        body: &str,
        ts: i64,
    ) -> rusqlite::Result<i64> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO messages (from_session, to_session, body, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![from, to, body, ts],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Returns `session`'s unread messages, oldest first. Unless `peek`, marks exactly those
    /// rows read (stamped `ts`) so a later call won't return them again — scoped to the ids just
    /// read, not a blanket `read_at IS NULL` update, so a message arriving between the select
    /// and the update isn't silently consumed.
    pub fn take_inbox(&self, session: &str, peek: bool, ts: i64) -> rusqlite::Result<Vec<Message>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT m.id, m.from_session, s.name, m.body, m.created_at
             FROM messages m
             LEFT JOIN sessions s ON s.id = m.from_session
             WHERE m.to_session = ?1 AND m.read_at IS NULL
             ORDER BY m.id ASC",
        )?;
        let msgs: Vec<Message> = stmt
            .query_map(params![session], |row| {
                Ok(Message {
                    id: row.get(0)?,
                    from_session: row.get(1)?,
                    from_name: row.get(2)?,
                    body: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        if !peek && !msgs.is_empty() {
            // ids are our own AUTOINCREMENT integers — safe to inline, no user input.
            let ids = msgs.iter().map(|m| m.id.to_string()).collect::<Vec<_>>().join(",");
            conn.execute(
                &format!("UPDATE messages SET read_at = ?1 WHERE id IN ({ids})"),
                params![ts],
            )?;
        }
        Ok(msgs)
    }

    /// The most recent `limit` inter-session messages, oldest-first, both endpoints resolved to
    /// display names — for the message-log panel's initial load.
    pub fn recent_messages(&self, limit: i64) -> rusqlite::Result<Vec<LogEntry>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT sf.name, st.name, m.body, m.created_at
             FROM messages m
             LEFT JOIN sessions sf ON sf.id = m.from_session
             LEFT JOIN sessions st ON st.id = m.to_session
             ORDER BY m.id DESC
             LIMIT ?1",
        )?;
        let mut rows: Vec<LogEntry> = stmt
            .query_map(params![limit], |row| {
                Ok(LogEntry {
                    from_name: row.get(0)?,
                    to_name: row.get(1)?,
                    body: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// `session id -> unread message count`, for sessions that currently have any.
    pub fn unread_counts(&self) -> rusqlite::Result<std::collections::HashMap<String, i64>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT to_session, COUNT(*) FROM messages WHERE read_at IS NULL GROUP BY to_session",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
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
            "SELECT ue.session_id, COALESCE(s.name, 'Outside TermHub'), ue.agent,
                    SUM(ue.tokens_in), SUM(ue.tokens_out)
             FROM usage_events ue
             LEFT JOIN sessions s ON s.id = ue.session_id
             GROUP BY COALESCE(ue.session_id, ''), ue.agent
             ORDER BY SUM(ue.tokens_in) + SUM(ue.tokens_out) DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SessionUsage {
                session_id: row.get(0)?,
                session_name: row.get(1)?,
                agent: row.get(2)?,
                tokens_in: row.get(3)?,
                tokens_out: row.get(4)?,
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
        // `'localtime'` (not just `'unixepoch'`) — without it SQLite buckets by UTC calendar
        // day, which silently disagrees with what the dashboard's "Today"/"Last 7 days" tiles
        // mean by "today" for anyone not at UTC+0 (e.g. a whole 9-hour-wide daily mismatch at
        // UTC+9). See `UsageDashboard.tsx`'s matching local-date fix.
        let mut stmt = conn.prepare(
            "SELECT strftime('%Y-%m-%d', timestamp, 'unixepoch', 'localtime') as day, agent,
                    SUM(tokens_in), SUM(tokens_out)
             FROM usage_events GROUP BY day, agent ORDER BY day DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(DayUsage {
                day: row.get(0)?,
                agent: row.get(1)?,
                tokens_in: row.get(2)?,
                tokens_out: row.get(3)?,
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

    pub fn get_setting(&self, key: &str) -> rusqlite::Result<Option<String>> {
        let conn = self.0.lock().unwrap();
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> rusqlite::Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(())
    }
}
