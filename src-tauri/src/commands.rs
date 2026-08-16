use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::Db;
use crate::external_terminal;
use crate::session::{default_cwd, default_shell, SessionInfo, SessionMeta};
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

    let meta = SessionMeta { id, name, cwd, shell, created_at };
    db.insert_session(&meta).map_err(|e| e.to_string())?;
    // Every opened folder counts toward the "Open Recent" MRU list, regardless of how the
    // session was created (new/duplicate/"new session here"/the Open Recent picker itself) —
    // see `touch_recent_folder`'s doc comment for why this is a separate table from `sessions`.
    db.touch_recent_folder(&meta.cwd, created_at).map_err(|e| e.to_string())?;

    Ok(SessionInfo { meta })
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
