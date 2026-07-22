use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;

use crate::session::SessionMeta;

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
}
