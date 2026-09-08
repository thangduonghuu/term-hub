use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::Db;
use crate::external_terminal;
use crate::session::{
    default_cwd, default_shell, SessionInfo, SessionMeta, SshAuthMethod, SshCredential, SshKey,
    SshKeySummary,
};
use crate::usage::UsageSummary;

/// A single native keyboard shortcut: which modifiers must be held, plus a raw macOS virtual
/// keycode (`NSEvent::keyCode()`) identifying the physical key — not a character, so this is
/// immune to layout/Shift changing what character a key produces (the previous, pre-
/// customization version of this app matched on `charactersIgnoringModifiers()` instead, which
/// worked but meant Cmd+Shift+`]` had to be special-cased as matching the character `}`).
/// `Copy`/`RootFolder` (and voice-dictation's PTT key — module doc) are handled with dedicated
/// `NSEvent` paths since they're one-shot/hold rather than "trigger an app action", but every
/// other shortcut in `SHORTCUT_ACTIONS` below is dispatched generically off exactly this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBinding {
    pub cmd: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub keycode: u16,
}

/// Every user-customizable native keyboard shortcut this app has, as `(action id, display
/// label, built-in default)` — single source of truth for Settings' "Keyboard Shortcuts" list
/// and for `TerminalInputView::key_down`'s dispatch (see `macos_input_view.rs`), which looks an
/// incoming keystroke up against whatever's currently bound to each of these ids rather than
/// hardcoding key comparisons per action. macOS virtual keycodes below are the standard,
/// stable, layout-position-based Mac constants (same ones already used elsewhere in this app
/// for Escape/Delete/Home/End/Page Up/Down) — not ASCII, not affected by Shift.
pub const SHORTCUT_ACTIONS: &[(&str, &str, KeyBinding)] = &[
    ("copy", "Copy", KeyBinding { cmd: true, ctrl: false, shift: false, alt: false, keycode: 0x08 }), // C
    ("paste", "Paste", KeyBinding { cmd: true, ctrl: false, shift: false, alt: false, keycode: 0x09 }), // V
    (
        "new_session",
        "New session",
        KeyBinding { cmd: true, ctrl: false, shift: false, alt: false, keycode: 0x11 }, // T
    ),
    (
        "close_session",
        "Close session",
        KeyBinding { cmd: true, ctrl: false, shift: false, alt: false, keycode: 0x0D }, // W
    ),
    (
        "next_session",
        "Next session",
        KeyBinding { cmd: true, ctrl: false, shift: true, alt: false, keycode: 0x1E }, // ]
    ),
    (
        "prev_session",
        "Previous session",
        KeyBinding { cmd: true, ctrl: false, shift: true, alt: false, keycode: 0x21 }, // [
    ),
    (
        "open_folder",
        "Open folder",
        KeyBinding { cmd: false, ctrl: true, shift: false, alt: false, keycode: 0x0F }, // R
    ),
];

fn shortcut_setting_key(action: &str) -> String {
    format!("keybind_{action}")
}

/// Every customizable shortcut's *effective* binding right now — the db override if the user's
/// ever changed it, otherwise `SHORTCUT_ACTIONS`' built-in default. Always returns exactly one
/// entry per `SHORTCUT_ACTIONS` entry, in the same order, so the frontend never has to separately
/// reason about "unset" — everything already has some binding.
pub fn get_shortcuts(db: &Db) -> Result<Vec<(String, String, KeyBinding)>, String> {
    SHORTCUT_ACTIONS
        .iter()
        .map(|&(id, label, default)| {
            let stored = db.get_setting(&shortcut_setting_key(id)).map_err(|e| e.to_string())?;
            let binding = stored
                .and_then(|s| serde_json::from_str::<KeyBinding>(&s).ok())
                .unwrap_or(default);
            Ok((id.to_string(), label.to_string(), binding))
        })
        .collect()
}

pub fn set_shortcut(db: &Db, action: &str, binding: KeyBinding) -> Result<(), String> {
    if !SHORTCUT_ACTIONS.iter().any(|&(id, _, _)| id == action) {
        return Err(format!("unknown shortcut action: {action}"));
    }
    let json = serde_json::to_string(&binding).map_err(|e| e.to_string())?;
    db.set_setting(&shortcut_setting_key(action), &json).map_err(|e| e.to_string())
}

/// Reverts one shortcut back to its `SHORTCUT_ACTIONS` default by removing the db override —
/// mirrors `clear_default_shell`'s "delete rather than write the default back" approach, so a
/// later change to what the built-in default *is* doesn't get masked by an old row that happens
/// to hold the previous default's value.
pub fn reset_shortcut(db: &Db, action: &str) -> Result<(), String> {
    db.delete_setting(&shortcut_setting_key(action)).map_err(|e| e.to_string())
}

pub fn get_default_cwd() -> String {
    default_cwd()
}

/// Native "Open Folder" dialog (VSCode-style) — lets the user pick any directory on disk to
/// open as a new session, rather than being limited to the default cwd or an already-open
/// session's folder. `None` if the user cancels. No `tauri-plugin-dialog` here: this app never
/// calls `tauri::Builder` (see `lib.rs::run()`), so there's no `AppHandle`/`Manager` for a Tauri
/// plugin to hang off of — `rfd` is a standalone crate that doesn't need one.
pub fn pick_folder() -> Option<String> {
    rfd::FileDialog::new().pick_folder().map(|p| p.to_string_lossy().to_string())
}

/// Terminal apps installed on this machine (e.g. iTerm2, Warp) that a session's folder can be
/// opened in as an alternative to the built-in native terminal.
pub fn list_terminal_apps() -> Vec<String> {
    external_terminal::list_apps()
}

const EXTERNAL_TERMINAL_APP_SETTING: &str = "external_terminal_app";

pub fn get_preferred_terminal_app(db: &Db) -> Result<Option<String>, String> {
    db.get_setting(EXTERNAL_TERMINAL_APP_SETTING).map_err(|e| e.to_string())
}

pub fn set_preferred_terminal_app(db: &Db, app: &str) -> Result<(), String> {
    db.set_setting(EXTERNAL_TERMINAL_APP_SETTING, app).map_err(|e| e.to_string())
}

pub fn open_external_terminal(app: &str, cwd: &str) -> Result<(), String> {
    external_terminal::open_external(app, cwd)
}

pub fn list_sessions(db: &Db) -> Result<Vec<SessionInfo>, String> {
    let metas = db.list_sessions().map_err(|e| e.to_string())?;
    Ok(metas.into_iter().map(|meta| SessionInfo { meta }).collect())
}

/// Recent inter-session messages for the log panel's initial load (see `control.rs` and
/// `MessageLog.tsx`). Live updates after mount come via the `termhub:message` DOM event.
pub fn get_message_log(db: &Db) -> Result<Vec<crate::message::LogEntry>, String> {
    db.recent_messages(200).map_err(|e| e.to_string())
}

/// `session id -> unread inter-session-message count`, polled by `App.tsx` alongside
/// `get_activity` to drive the sidebar's per-session unread badge.
pub fn get_unread_counts(db: &Db) -> Result<std::collections::HashMap<String, i64>, String> {
    db.unread_counts().map_err(|e| e.to_string())
}

/// Whether an arrival toast pops in the sidebar when this session receives an inter-session
/// message (Settings > Messaging). Stored as `"1"` / `"0"`; absent means on.
pub fn get_message_toast_enabled(db: &Db) -> Result<bool, String> {
    Ok(db.get_setting("intersession_toast").map_err(|e| e.to_string())?.as_deref() != Some("0"))
}

pub fn set_message_toast_enabled(db: &Db, enabled: bool) -> Result<(), String> {
    db.set_setting("intersession_toast", if enabled { "1" } else { "0" }).map_err(|e| e.to_string())
}

/// Whether an inter-session message for a session in this window is typed straight into that
/// session's terminal (Settings > Messaging) — for a keyboard-driven agent that doesn't poll
/// `check_inbox`. Stored as `"1"` / `"0"`; absent means off (this drives the target's input, so
/// it's opt-in, unlike the toast).
pub fn get_message_autodeliver_enabled(db: &Db) -> Result<bool, String> {
    Ok(db.get_setting("intersession_autodeliver").map_err(|e| e.to_string())?.as_deref() == Some("1"))
}

pub fn set_message_autodeliver_enabled(db: &Db, enabled: bool) -> Result<(), String> {
    db.set_setting("intersession_autodeliver", if enabled { "1" } else { "0" })
        .map_err(|e| e.to_string())
}

/// Whether an agent in another session may run shell commands in a session in this window via
/// the `run_in_session` MCP tool / `termhub-msg run` (Settings > Messaging). Stored as
/// `"1"` / `"0"`; absent means off — this executes arbitrary commands in the target, so it's
/// opt-in. See `control.rs`'s `dispatch_run`.
pub fn get_message_run_enabled(db: &Db) -> Result<bool, String> {
    Ok(db.get_setting("intersession_run").map_err(|e| e.to_string())?.as_deref() == Some("1"))
}

pub fn set_message_run_enabled(db: &Db, enabled: bool) -> Result<(), String> {
    db.set_setting("intersession_run", if enabled { "1" } else { "0" }).map_err(|e| e.to_string())
}

/// The `claude mcp add …` line for the Settings > Messaging copy-button — registers the
/// `termhub-msg mcp` stdio server (see `bin/termhub-msg.rs`) with Claude Code. Uses the
/// absolute path to the CLI, which ships next to the GUI binary, so it also works run from a
/// plain shell outside a session. `None` on the (unsupported) platforms where the CLI can't
/// resolve its own location.
pub fn get_mcp_register_command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("no parent directory for the running executable")?;
    let cli = dir.join("termhub-msg");
    Ok(format!("claude mcp add termhub-msg -- {} mcp", cli.display()))
}

pub fn create_session(
    db: &Db,
    name: Option<String>,
    cwd: Option<String>,
) -> Result<SessionInfo, String> {
    let id = Uuid::new_v4().to_string();
    let cwd = cwd.unwrap_or_else(default_cwd);
    // The settings-table override (see `get_default_shell`/`set_default_shell`) wins if set,
    // otherwise fall back to $SHELL/COMSPEC same as before.
    let shell = db
        .get_setting(DEFAULT_SHELL_SETTING)
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(default_shell);
    let name = name.unwrap_or_else(|| "Session".to_string());
    let created_at = unix_now();

    let meta =
        SessionMeta { id, name, cwd, shell, shell_args: Vec::new(), ssh_credential_id: None, created_at };
    db.insert_session(&meta).map_err(|e| e.to_string())?;
    // Every opened folder counts toward the "Open Recent" MRU list, regardless of how the
    // session was created (new/duplicate/"new session here"/the Open Recent picker itself) —
    // see `touch_recent_folder`'s doc comment for why this is a separate table from `sessions`.
    db.touch_recent_folder(&meta.cwd, created_at).map_err(|e| e.to_string())?;

    Ok(SessionInfo { meta })
}

/// Native "open a file" dialog for importing a private key into the vault (`SshConnect.tsx`'s
/// "Add new key" form) — reads and returns the file's *content*, not its path: the path is
/// discarded the instant this returns, since the vault stores key bytes, not a filesystem
/// reference (see `SshKey`'s doc comment). Same standalone-`rfd` reasoning as `pick_folder` (no
/// `AppHandle` to hang a `tauri-plugin-dialog` off of here). `None` on cancel or a read failure
/// (e.g. a binary/non-UTF8 file, which a private key file never legitimately is).
pub fn read_key_file() -> Option<String> {
    let path = rfd::FileDialog::new().pick_file()?;
    std::fs::read_to_string(path).ok()
}

#[allow(clippy::too_many_arguments)]
pub fn create_ssh_credential(
    db: &Db,
    label: String,
    host: String,
    port: u16,
    username: String,
    auth_method: SshAuthMethod,
    password: Option<String>,
    key_id: Option<String>,
) -> Result<SshCredential, String> {
    let password = password.filter(|s| !s.is_empty());
    let key_id = key_id.filter(|s| !s.trim().is_empty());
    match auth_method {
        SshAuthMethod::Password if password.is_none() => {
            return Err("password auth requires a password".to_string())
        }
        SshAuthMethod::Key if key_id.is_none() => {
            return Err("key auth requires a saved key".to_string())
        }
        _ => {}
    }
    let cred = SshCredential {
        id: Uuid::new_v4().to_string(),
        label,
        host,
        port,
        username,
        auth_method,
        password,
        key_id,
        created_at: unix_now(),
    };
    db.insert_ssh_credential(&cred).map_err(|e| e.to_string())?;
    Ok(cred)
}

pub fn list_ssh_credentials(db: &Db) -> Result<Vec<SshCredential>, String> {
    db.list_ssh_credentials().map_err(|e| e.to_string())
}

pub fn delete_ssh_credential(db: &Db, id: &str) -> Result<(), String> {
    db.delete_ssh_credential(id).map_err(|e| e.to_string())
}

pub fn create_ssh_key(db: &Db, name: String, content: String) -> Result<SshKeySummary, String> {
    let content = content.trim();
    if content.is_empty() {
        return Err("key content is empty".to_string());
    }
    let key = SshKey {
        id: Uuid::new_v4().to_string(),
        name,
        content: content.to_string(),
        created_at: unix_now(),
    };
    db.insert_ssh_key(&key).map_err(|e| e.to_string())?;
    write_key_file(&key.id, &key.content)?;
    Ok(SshKeySummary { id: key.id, name: key.name, created_at: key.created_at })
}

pub fn list_ssh_keys(db: &Db) -> Result<Vec<SshKeySummary>, String> {
    db.list_ssh_keys().map_err(|e| e.to_string())
}

pub fn delete_ssh_key(db: &Db, id: &str) -> Result<(), String> {
    db.delete_ssh_key(id).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(key_file_path(id));
    Ok(())
}

/// Where a vault key's content is materialized on disk purely so `ssh -i` has a file path to
/// point at — `ssh` has no way to take a private key as inline bytes. Lives under the app's own
/// data dir, entirely managed by TermHub: nothing in the UI ever shows the user this path or
/// asks them to pick one (that's the whole point of the vault vs. the old per-credential
/// identity-path field).
fn key_file_path(key_id: &str) -> std::path::PathBuf {
    crate::app_data_dir().join("ssh_keys").join(key_id)
}

/// Writes (or rewrites) a key's materialized file with `0600` permissions — `ssh` refuses a
/// private key file that's group/world-readable. Called both when a key is first added and
/// defensively before every connect (`connect_ssh_session`), so a file removed or corrupted
/// out-of-band (e.g. the app data dir was manually cleared) self-heals from the db, which stays
/// the source of truth.
fn write_key_file(key_id: &str, content: &str) -> Result<(), String> {
    let path = key_file_path(key_id);
    let dir = path.parent().ok_or("no parent directory for the key file")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(&path, content).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Builds and saves a new session that runs `ssh` straight into a saved credential instead of a
/// local shell — reuses the exact same session-tiling machinery as `create_session` (sidebar
/// entry, persistence, focus, close, reconnect-on-restart) so it opens as a tile just like any
/// other session; only `shell`/`shell_args` differ. Deliberately doesn't call
/// `touch_recent_folder`: every SSH session shares the same local `cwd` (`default_cwd()`, since
/// `ssh` itself ignores it beyond where the local process starts), and that folder isn't a real
/// project directory the "Open Recent" picker should start suggesting.
///
/// Password-auth credentials are spawned as a plain `ssh` with no special handling of their
/// own — `password` isn't fed to `ssh` itself (see `SshCredential::password`'s doc comment), so
/// the real login prompt still appears in the tile exactly like it would in any other terminal.
/// It's typed in automatically once that prompt shows up, via the mechanism described below.
///
/// Returns the new session alongside the credential's saved password (only when its auth
/// method is `Password`) — the caller (`ipc.rs`'s `connect_ssh`) stashes that into `App`'s
/// `pending_ssh_passwords` so it can be typed in the instant `ssh`'s own prompt appears (see
/// that field's doc comment in `lib.rs`). Never part of `SessionInfo` itself — that's what's
/// serialized back to the frontend, and a saved password has no business reaching the webview.
pub fn connect_ssh_session(
    db: &Db,
    credential_id: &str,
) -> Result<(SessionInfo, Option<String>), String> {
    let cred = db.get_ssh_credential(credential_id).map_err(|e| e.to_string())?;
    let password = if cred.auth_method == SshAuthMethod::Password { cred.password.clone() } else { None };
    let mut shell_args = Vec::new();
    if cred.auth_method == SshAuthMethod::Key {
        if let Some(key_id) = &cred.key_id {
            let key = db.get_ssh_key(key_id).map_err(|e| e.to_string())?;
            write_key_file(key_id, &key.content)?;
            shell_args.push("-i".to_string());
            shell_args.push(key_file_path(key_id).to_string_lossy().to_string());
        }
    }
    shell_args.push("-p".to_string());
    shell_args.push(cred.port.to_string());
    shell_args.push(format!("{}@{}", cred.username, cred.host));

    let meta = SessionMeta {
        id: Uuid::new_v4().to_string(),
        name: cred.label,
        cwd: default_cwd(),
        shell: "ssh".to_string(),
        shell_args,
        ssh_credential_id: Some(credential_id.to_string()),
        created_at: unix_now(),
    };
    db.insert_session(&meta).map_err(|e| e.to_string())?;
    Ok((SessionInfo { meta }, password))
}

/// The password to re-arm for a restored SSH session — same lookup `connect_ssh_session` itself
/// does, just keyed off the session's own saved `ssh_credential_id` instead of a fresh pick from
/// the "Connect to VPS" picker. Used on app-launch reconnect and on reviving a dead tile
/// (`lib.rs`'s `respawn_active_if_exited`) so either one re-authenticates exactly like the
/// original `connect_ssh` did, rather than landing back on an unanswered password prompt.
/// `None` for an ordinary (non-SSH) session, a key-auth one (the key file `-i` already points at
/// is baked into `shell_args` and needs no re-arming), or one whose credential was since deleted.
pub fn ssh_reconnect_password(db: &Db, meta: &SessionMeta) -> Option<String> {
    let credential_id = meta.ssh_credential_id.as_ref()?;
    let cred = db.get_ssh_credential(credential_id).ok()?;
    if cred.auth_method == SshAuthMethod::Password { cred.password } else { None }
}

/// Folders previously opened as a session, most-recent first, for the "Open Recent" picker
/// (VSCode's Cmd+R equivalent here is Ctrl+R — see `macos_input_view.rs`).
pub fn list_recent_folders(db: &Db) -> Result<Vec<String>, String> {
    db.list_recent_folders().map_err(|e| e.to_string())
}

/// Removes one folder from the "Open Recent" list without touching any session open in it.
pub fn remove_recent_folder(db: &Db, path: &str) -> Result<(), String> {
    db.remove_recent_folder(path).map_err(|e| e.to_string())
}

pub fn rename_session(db: &Db, id: &str, name: &str) -> Result<(), String> {
    db.rename_session(id, name).map_err(|e| e.to_string())
}

/// Removes the session from the saved list.
pub fn close_session(db: &Db, id: &str) -> Result<(), String> {
    db.delete_session(id).map_err(|e| e.to_string())
}

pub fn get_usage_summary(db: &Db) -> Result<UsageSummary, String> {
    let per_session = db.usage_per_session().map_err(|e| e.to_string())?;
    let per_agent = db.usage_per_agent().map_err(|e| e.to_string())?;
    let per_day = db.usage_per_day().map_err(|e| e.to_string())?;
    let (total_tokens_in, total_tokens_out) = db.usage_grand_total().map_err(|e| e.to_string())?;
    Ok(UsageSummary { per_session, per_agent, per_day, total_tokens_in, total_tokens_out })
}

const DEFAULT_SHELL_SETTING: &str = "default_shell";

/// The configured default-shell override, or `None` if unset (new sessions fall back to
/// `$SHELL`/`COMSPEC` — see `create_session`). Distinct from `get_default_cwd`'s `default_cwd()`
/// pairing: that one has no settings-table override yet, this one does.
pub fn get_default_shell(db: &Db) -> Result<Option<String>, String> {
    db.get_setting(DEFAULT_SHELL_SETTING).map_err(|e| e.to_string())
}

pub fn set_default_shell(db: &Db, shell: &str) -> Result<(), String> {
    db.set_setting(DEFAULT_SHELL_SETTING, shell).map_err(|e| e.to_string())
}

pub fn clear_default_shell(db: &Db) -> Result<(), String> {
    db.delete_setting(DEFAULT_SHELL_SETTING).map_err(|e| e.to_string())
}

const ACCENT_COLOR_SETTING: &str = "accent_color";

/// The built-in accent color — matches the sidebar's own default (`.session-item.active`'s
/// `border-color` in App.css) so the native terminal border and the webview sidebar agree
/// out of the box, before the user ever opens Settings.
pub const DEFAULT_ACCENT_COLOR: &str = "#d8a657";

/// The configured accent color as a `#rrggbb` hex string, shared by the sidebar's active-
/// session highlight (CSS `--accent-color`) and the native active-tile border (`terminal.rs`'s
/// `render_border`) — `None` if never customized, in which case both sides fall back to
/// `DEFAULT_ACCENT_COLOR` independently.
pub fn get_accent_color(db: &Db) -> Result<Option<String>, String> {
    db.get_setting(ACCENT_COLOR_SETTING).map_err(|e| e.to_string())
}

pub fn set_accent_color(db: &Db, color: &str) -> Result<(), String> {
    if parse_hex_color(color).is_none() {
        return Err(format!("invalid color: {color}"));
    }
    db.set_setting(ACCENT_COLOR_SETTING, color).map_err(|e| e.to_string())
}

/// Parses a `#rrggbb` hex string (case-insensitive, exactly what an `<input type="color">`
/// produces) into `wgpu`-ready `0.0..=1.0` RGB — `None` for anything else, so a malformed or
/// tampered-with value from the db never reaches `set_setting`/the live border renderer.
pub fn parse_hex_color(s: &str) -> Option<[f32; 3]> {
    let s = s.strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0])
}

const VOICE_PTT_KEYCODE_SETTING: &str = "voice_ptt_keycode";

/// The configured push-to-talk key for voice dictation (see `speech.rs`) — a raw macOS virtual
/// keycode (`NSEvent::keyCode`) for one of a curated set of modifier keys the Settings panel
/// offers (right Option, left Option, right Shift, etc.), stored as its decimal string form.
/// `None` if never set, in which case the caller falls back to the built-in default (right
/// Option — see `macos_input_view::DEFAULT_PTT_KEYCODE`). Deliberately restricted to modifier
/// keys at the UI layer: those are the only physical keys whose press/release AppKit reports
/// reliably regardless of what else is held (see `TerminalInputView::flags_changed`'s doc
/// comment for the confirmed real bug — held Cmd swallowing a combo'd key's `keyUp:` — that
/// ruled out anything else).
pub fn get_voice_ptt_keycode(db: &Db) -> Result<Option<u16>, String> {
    let raw = db.get_setting(VOICE_PTT_KEYCODE_SETTING).map_err(|e| e.to_string())?;
    Ok(raw.and_then(|s| s.parse().ok()))
}

pub fn set_voice_ptt_keycode(db: &Db, keycode: u16) -> Result<(), String> {
    db.set_setting(VOICE_PTT_KEYCODE_SETTING, &keycode.to_string()).map_err(|e| e.to_string())
}

const LUMEN_PROMPT_SEEN_SETTING: &str = "lumen_prompt_seen";

/// Whether the "try Lumen" sidebar promo (see `LumenPromo.tsx`) has already been dismissed or
/// acted on — it's a one-time, first-launch suggestion, not something to keep nagging about on
/// every session's startup.
pub fn has_seen_lumen_prompt(db: &Db) -> Result<bool, String> {
    Ok(db.get_setting(LUMEN_PROMPT_SEEN_SETTING).map_err(|e| e.to_string())?.is_some())
}

pub fn mark_lumen_prompt_seen(db: &Db) -> Result<(), String> {
    db.set_setting(LUMEN_PROMPT_SEEN_SETTING, "1").map_err(|e| e.to_string())
}

/// Reads the system clipboard's text, for the "Connect to VPS" form's own Cmd+V handling
/// (`SshConnect.tsx`) — the sidebar webview is embedded in a nonstandard way (a child `NSView`
/// of the app's own window rather than a real Tauri-owned one — see `ipc.rs`'s module doc), and
/// this app also has no menu bar (see `macos_input_view.rs`'s `key_down` doc comment on why
/// Cmd+-combos need explicit handling at all here), so nothing can be assumed about whether a
/// plain OS-level paste into a focused HTML input reaches it reliably in this setup. Routing
/// through an explicit read here — same already-proven `arboard` crate `AppEvent::Copy`/`Paste`
/// already use for the terminal's own clipboard handling — sidesteps that uncertainty entirely
/// instead of depending on it. `None` if the clipboard is empty, unreadable, or holds no text.
pub fn read_clipboard_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

/// Opens a URL in the user's default browser — same plain-process-spawning approach as
/// `external_terminal::open_external` (this app never calls `tauri::Builder`, so there's no
/// `AppHandle` for `tauri-plugin-opener` to hang off of).
pub fn open_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    result.map_err(|e| e.to_string())?;
    Ok(())
}

const ANTHROPIC_API_KEY_SETTING: &str = "anthropic_api_key";

pub fn has_anthropic_api_key(db: &Db) -> Result<bool, String> {
    Ok(db.get_setting(ANTHROPIC_API_KEY_SETTING).map_err(|e| e.to_string())?.is_some())
}

pub fn set_anthropic_api_key(db: &Db, key: &str) -> Result<(), String> {
    db.set_setting(ANTHROPIC_API_KEY_SETTING, key).map_err(|e| e.to_string())
}

pub fn clear_anthropic_api_key(db: &Db) -> Result<(), String> {
    db.delete_setting(ANTHROPIC_API_KEY_SETTING).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
pub struct ClaudeLimits {
    /// (header name with the "anthropic-ratelimit-" prefix stripped, value) pairs, in
    /// whatever set Anthropic actually returns — kept as raw pairs rather than a fixed
    /// struct since the exact header set isn't publicly guaranteed to be stable.
    pub limits: Vec<(String, String)>,
}

/// Makes one minimal (~1 output token) real request to Anthropic's Messages API purely to
/// read back its `anthropic-ratelimit-*` response headers. This is the org/API-key rate
/// limit (requests & tokens per minute) — a different quota than Claude Code's Pro/Max
/// 5-hour session limit, which has no public API and isn't available here.
///
/// Blocking (not async): the IPC layer already runs every command on its own background
/// thread (see ipc.rs), so there's no event loop to avoid blocking here.
pub fn check_claude_limits(db: &Db) -> Result<ClaudeLimits, String> {
    let key = db
        .get_setting(ANTHROPIC_API_KEY_SETTING)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "No Anthropic API key configured".to_string())?;

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "model": "claude-3-5-haiku-latest",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .map_err(|e| e.to_string())?;

    let mut limits = Vec::new();
    for (name, value) in resp.headers().iter() {
        let name_str = name.as_str();
        if let Some(stripped) = name_str.strip_prefix("anthropic-ratelimit-") {
            if let Ok(v) = value.to_str() {
                limits.push((stripped.to_string(), v.to_string()));
            }
        }
    }

    if limits.is_empty() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(format!("No rate-limit headers in response (status {status}): {body}"));
    }

    Ok(ClaudeLimits { limits })
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    // Verifies `read_clipboard_text` against the real macOS pasteboard rather than just
    // round-tripping through `arboard`'s own setter — `pbcopy` is a separate, independent path
    // onto the same pasteboard, so this actually exercises "can this app's clipboard-read
    // command see what an external paste source put there," which is the exact thing
    // `SshConnect.tsx`'s Cmd+V handling depends on.
    #[test]
    fn read_clipboard_text_reads_whats_on_the_pasteboard() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let marker = format!("termhub-clipboard-test-{}", std::process::id());
        let mut child = Command::new("pbcopy")
            .stdin(Stdio::piped())
            .spawn()
            .expect("pbcopy should be available on macOS");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(marker.as_bytes())
            .expect("write to pbcopy");
        child.wait().expect("pbcopy should exit cleanly");

        assert_eq!(read_clipboard_text().as_deref(), Some(marker.as_str()));
    }
}
