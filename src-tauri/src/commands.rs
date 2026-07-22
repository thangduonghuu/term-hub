use std::sync::Arc;
use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::db::Db;
use crate::external_terminal;
use crate::pty_manager::PtyManager;
use crate::session::{default_cwd, default_shell, SessionInfo, SessionMeta};
use crate::usage::UsageSummary;

#[tauri::command]
pub fn get_default_cwd() -> String {
    default_cwd()
}

/// Terminal apps installed on this machine (e.g. iTerm2, Warp) that a session's folder
/// can be opened in as an alternative to the built-in embedded terminal.
#[tauri::command]
pub fn list_terminal_apps() -> Vec<String> {
    external_terminal::list_apps()
}

#[tauri::command]
pub fn open_external_terminal(app: String, cwd: String) -> Result<(), String> {
    external_terminal::open_external(&app, &cwd)
}

#[tauri::command]
pub fn list_sessions(db: State<Arc<Db>>, manager: State<PtyManager>) -> Result<Vec<SessionInfo>, String> {
    let metas = db.list_sessions().map_err(|e| e.to_string())?;
    Ok(metas
        .into_iter()
        .map(|meta| {
            let running = manager.is_running(&meta.id);
            SessionInfo { meta, running }
        })
        .collect())
}

#[tauri::command]
pub fn create_session(
    app: AppHandle,
    db: State<Arc<Db>>,
    manager: State<PtyManager>,
    name: Option<String>,
    cwd: Option<String>,
) -> Result<SessionInfo, String> {
    let id = Uuid::new_v4().to_string();
    let cwd = cwd.unwrap_or_else(default_cwd);
    let shell = default_shell();
    let name = name.unwrap_or_else(|| "Session".to_string());
    let created_at = unix_now();

    let meta = SessionMeta {
        id: id.clone(),
        name,
        cwd: cwd.clone(),
        shell: shell.clone(),
        created_at,
    };

    manager.spawn(&app, id.clone(), &cwd, &shell)?;
    db.insert_session(&meta).map_err(|e| e.to_string())?;

    Ok(SessionInfo {
        meta,
        running: true,
    })
}

#[tauri::command]
pub fn reopen_session(
    app: AppHandle,
    db: State<Arc<Db>>,
    manager: State<PtyManager>,
    id: String,
) -> Result<SessionInfo, String> {
    let meta = db.get_session(&id).map_err(|e| e.to_string())?;
    if !manager.is_running(&id) {
        manager.spawn(&app, id.clone(), &meta.cwd, &meta.shell)?;
    }
    Ok(SessionInfo {
        meta,
        running: true,
    })
}

#[tauri::command]
pub fn write_pty(manager: State<PtyManager>, id: String, data: String) -> Result<(), String> {
    manager.write(&id, &data)
}

#[tauri::command]
pub fn resize_pty(manager: State<PtyManager>, id: String, rows: u16, cols: u16) -> Result<(), String> {
    manager.resize(&id, rows, cols)
}

#[tauri::command]
pub fn rename_session(db: State<Arc<Db>>, id: String, name: String) -> Result<(), String> {
    db.rename_session(&id, &name).map_err(|e| e.to_string())
}

/// Ends the pty process and permanently forgets the session. Distinct from quitting the app,
/// which leaves saved sessions in the DB so they can be reopened next launch.
#[tauri::command]
pub fn close_session(db: State<Arc<Db>>, manager: State<PtyManager>, id: String) -> Result<(), String> {
    manager.kill(&id);
    db.delete_session(&id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_usage_summary(db: State<Arc<Db>>) -> Result<UsageSummary, String> {
    let per_session = db.usage_per_session().map_err(|e| e.to_string())?;
    let per_agent = db.usage_per_agent().map_err(|e| e.to_string())?;
    let per_day = db.usage_per_day().map_err(|e| e.to_string())?;
    let (total_tokens_in, total_tokens_out) = db.usage_grand_total().map_err(|e| e.to_string())?;
    Ok(UsageSummary {
        per_session,
        per_agent,
        per_day,
        total_tokens_in,
        total_tokens_out,
    })
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
