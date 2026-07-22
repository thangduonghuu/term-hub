mod commands;
mod db;
mod external_terminal;
mod pty_manager;
mod session;
mod usage;

use db::Db;
use pty_manager::PtyManager;
use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&app_data_dir)?;
            let db = Arc::new(
                Db::open(&app_data_dir.join("termhub.sqlite")).map_err(|e| e.to_string())?,
            );
            usage::spawn_tracker(db.clone());
            app.manage(db);
            app.manage(PtyManager::new());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_default_cwd,
            commands::list_terminal_apps,
            commands::open_external_terminal,
            commands::list_sessions,
            commands::create_session,
            commands::reopen_session,
            commands::write_pty,
            commands::resize_pty,
            commands::rename_session,
            commands::close_session,
            commands::get_usage_summary,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
