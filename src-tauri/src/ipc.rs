//! Minimal replacement for Tauri's `invoke`/`#[tauri::command]` machinery.
//!
//! Dropping to raw `wry` (so the sidebar webview can be embedded as a *child* of our own
//! native window — see `lib.rs`) forfeits Tauri's built-in IPC. This is the hand-rolled
//! stand-in: the frontend's `window.ipc.postMessage(json)` delivers a `{id, cmd, args}`
//! request here (via `WebViewBuilder::with_ipc_handler`), each request runs on its own
//! background thread (so a slow command, e.g. `check_claude_limits`'s network call, never
//! blocks the window's event loop), and the result is marshaled back to the main thread via
//! `winit`'s `EventLoopProxy` to call `webview.evaluate_script(...)` — `WebView` methods
//! aren't safe to call off the main thread.
//!
//! Frontend counterpart: `src/lib/ipc.ts`.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;
use winit::event_loop::EventLoopProxy;

use crate::commands;
use crate::db::Db;
use crate::{Activity, AppEvent, Exited};

#[derive(Deserialize)]
struct IpcRequest {
    id: u64,
    cmd: String,
    args: Value,
}

/// Called synchronously from `with_ipc_handler` (main thread). Spawns the actual work on a
/// background thread and returns immediately.
pub fn spawn_dispatch(
    db: Arc<Db>,
    activity: Activity,
    exited: Exited,
    proxy: EventLoopProxy<AppEvent>,
    raw: &str,
) {
    let Ok(req) = serde_json::from_str::<IpcRequest>(raw) else {
        return;
    };
    let proxy_for_handle = proxy.clone();
    std::thread::spawn(move || {
        let result = handle(&db, &activity, &exited, &proxy_for_handle, &req.cmd, req.args);
        let payload = match result {
            Ok(data) => serde_json::json!({"ok": true, "data": data}),
            Err(error) => serde_json::json!({"ok": false, "error": error}),
        };
        let script = format!(
            "window.__ipcResolve({}, {})",
            req.id,
            serde_json::to_string(&payload).unwrap_or_else(|_| "null".into())
        );
        let _ = proxy.send_event(AppEvent::IpcResponse(script));
    });
}

/// `db` mutations here (create/close/focus) also need to spawn/kill/focus the actual live
/// pty-backed session (Phase 3: multi-session tiling) — that state lives in `App`, on the
/// main thread, unreachable from this background dispatch thread, so those commands notify
/// it via `proxy` after the db write succeeds. `activity` (Phase 4's sidebar activity dot) is
/// simpler — just a shared map `App` also writes to, no round-trip through the event loop
/// needed to read it.
fn handle(
    db: &Db,
    activity: &Activity,
    exited: &Exited,
    proxy: &EventLoopProxy<AppEvent>,
    cmd: &str,
    args: Value,
) -> Result<Value, String> {
    fn to_value<T: serde::Serialize>(v: T) -> Result<Value, String> {
        serde_json::to_value(v).map_err(|e| e.to_string())
    }
    fn arg<T: serde::de::DeserializeOwned>(args: &Value, key: &str) -> Option<T> {
        args.get(key).cloned().and_then(|v| serde_json::from_value(v).ok())
    }

    match cmd {
        "get_default_cwd" => to_value(commands::get_default_cwd()),
        "list_terminal_apps" => to_value(commands::list_terminal_apps()),
        "get_preferred_terminal_app" => commands::get_preferred_terminal_app(db).and_then(to_value),
        "set_preferred_terminal_app" => {
            let app: String = arg(&args, "app").ok_or("missing app")?;
            commands::set_preferred_terminal_app(db, &app).and_then(to_value)
        }
        "open_external_terminal" => {
            let app: String = arg(&args, "app").ok_or("missing app")?;
            let cwd: String = arg(&args, "cwd").ok_or("missing cwd")?;
            commands::open_external_terminal(&app, &cwd).and_then(to_value)
        }
        "list_sessions" => commands::list_sessions(db).and_then(to_value),
        "create_session" => {
            let name: Option<String> = arg(&args, "name");
            let cwd: Option<String> = arg(&args, "cwd");
            let info = commands::create_session(db, name, cwd)?;
            let _ = proxy.send_event(AppEvent::SpawnSession {
                id: info.meta.id.clone(),
                cwd: info.meta.cwd.clone(),
                shell: info.meta.shell.clone(),
            });
            to_value(info)
        }
        "rename_session" => {
            let id: String = arg(&args, "id").ok_or("missing id")?;
            let name: String = arg(&args, "name").ok_or("missing name")?;
            commands::rename_session(db, &id, &name).and_then(to_value)
        }
        "close_session" => {
            let id: String = arg(&args, "id").ok_or("missing id")?;
            commands::close_session(db, &id)?;
            let _ = proxy.send_event(AppEvent::CloseSession { id });
            to_value(())
        }
        "focus_session" => {
            let id: String = arg(&args, "id").ok_or("missing id")?;
            let _ = proxy.send_event(AppEvent::FocusSession(id));
            to_value(())
        }
        "get_activity" => {
            let map = activity.lock().map_err(|_| "activity lock poisoned".to_string())?;
            to_value(map.clone())
        }
        // Phase 5: ids of sessions whose shell process has exited, for the sidebar's dead-tile
        // indicator — same shared-map-no-event-loop-roundtrip pattern as `get_activity` above.
        "get_exited_sessions" => {
            let set = exited.lock().map_err(|_| "exited lock poisoned".to_string())?;
            to_value(set.iter().cloned().collect::<Vec<String>>())
        }
        // Any full-window-centered modal (usage dashboard, settings) rendered inside the
        // sidebar webview, which is normally kept narrow (just the sidebar strip) so clicks
        // past it fall through to the native terminal tiles — see `App.webview_full`'s doc
        // comment. Widen it only while a modal is actually open.
        "set_overlay_open" => {
            let open: bool = arg(&args, "open").ok_or("missing open")?;
            let _ = proxy.send_event(AppEvent::SetOverlayOpen(open));
            to_value(())
        }
        "get_default_shell" => commands::get_default_shell(db).and_then(to_value),
        "set_default_shell" => {
            let shell: String = arg(&args, "shell").ok_or("missing shell")?;
            commands::set_default_shell(db, &shell).and_then(to_value)
        }
        "clear_default_shell" => commands::clear_default_shell(db).and_then(to_value),
        "get_usage_summary" => commands::get_usage_summary(db).and_then(to_value),
        "has_anthropic_api_key" => commands::has_anthropic_api_key(db).and_then(to_value),
        "set_anthropic_api_key" => {
            let key: String = arg(&args, "key").ok_or("missing key")?;
            commands::set_anthropic_api_key(db, &key).and_then(to_value)
        }
        "clear_anthropic_api_key" => commands::clear_anthropic_api_key(db).and_then(to_value),
        "check_claude_limits" => commands::check_claude_limits(db).and_then(to_value),
        _ => Err(format!("unknown command: {cmd}")),
    }
}
