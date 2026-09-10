mod commands;
#[cfg(unix)]
mod control;
mod db;
mod external_terminal;
mod ipc;
mod message;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_input_view;
mod session;
#[cfg(target_os = "macos")]
mod speech;
mod terminal;
mod usage;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use db::Db;
use terminal::{Frame, TerminalSession, TextPipeline};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::{Rect, WebView, WebViewBuilder};

const SIDEBAR_WIDTH: f64 = 220.0;
// The right-docked inter-session message log (`MessageLog.tsx`), its own webview. Three states
// (`App.log_panel`): fully off-screen until the first message ever arrives, then a
// `LOG_HANDLE_WIDTH` chat-button rail, expandable to the full `LOG_PANEL_WIDTH`. `tile_rects`
// reserves whatever it currently occupies off the right edge of the terminal area.
const LOG_PANEL_WIDTH: f64 = 300.0;
const LOG_HANDLE_WIDTH: f64 = 36.0;

/// How much of the message-log webview is on screen — see `LOG_PANEL_WIDTH` / `App.log_panel`.
#[derive(Clone, Copy, PartialEq)]
enum LogPanel {
    /// No message has ever been logged this run — nothing shown.
    Hidden,
    /// The chat-button rail only.
    Collapsed,
    /// The full panel.
    Open,
}

// The sidebar webview served `dev_server_url()` unconditionally in every build, dev and
// release alike — fine under `tauri dev` (Vite is actually listening on :1420), but a packaged
// `.app` launched standalone has no dev server, so the webview silently failed to load and the
// sidebar rendered blank (no visible navigation strip, though `SIDEBAR_WIDTH` was still being
// reserved in the tile layout). Release builds instead serve the `npm run build` output
// (`../dist`, matching `frontendDist` in tauri.conf.json) embedded directly in the binary via a
// custom `termhub://` protocol, the same approach Tauri's own asset protocol uses.
#[cfg(not(debug_assertions))]
mod assets {
    #[derive(rust_embed::RustEmbed)]
    #[folder = "../dist"]
    pub struct Assets;

    pub fn mime_of(path: &str) -> &'static str {
        match path.rsplit('.').next().unwrap_or("") {
            "html" => "text/html",
            "js" | "mjs" => "text/javascript",
            "css" => "text/css",
            "json" => "application/json",
            "svg" => "image/svg+xml",
            "png" => "image/png",
            "ico" => "image/x-icon",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            _ => "application/octet-stream",
        }
    }
}

/// Session id -> unix-epoch milliseconds of its last pty output — shared (not just an `App`
/// field) because the sidebar's `get_activity` IPC command (Phase 4's activity dot) reads it
/// from a background dispatch thread (`ipc.rs`), while `App::user_event` writes it on the main
/// thread whenever `AppEvent::PtyOutput` arrives.
type Activity = Arc<Mutex<HashMap<String, u64>>>;

/// Ids of sessions whose pty-backed shell process has exited (Phase 5) — shared with `ipc.rs`'s
/// `get_exited_sessions` command the same way `Activity` is, so the sidebar can poll it from a
/// background dispatch thread while `App::user_event` writes to it on the main thread whenever
/// `AppEvent::SessionExited` arrives.
type Exited = Arc<Mutex<HashSet<String>>>;

/// The currently-focused tile's session id, shared with `ipc.rs`'s `get_active_session`
/// command — a one-shot snapshot the frontend pulls once on mount to seed its own `activeId`
/// state with whichever tile `App::resumed` picked at startup (`self.terms.first()`), which the
/// frontend has no other way to learn (unlike sidebar-driven focus changes, where the frontend
/// already sets its own state before ever asking Rust). Not a live mirror of `active_id` after
/// that: later changes reach the frontend via the `termhub:session-focused` push event instead
/// (see `App::report_active_session`), so nothing re-reads this afterward.
type ActiveSession = Arc<Mutex<Option<String>>>;

/// One tile's change-detection key for `App.last_frames` — id, per-cell glyphs (with color),
/// cursor cell/shape, selected cells, background-colored cells, and exited state.
/// `RedrawRequested` skips a tile entirely when none of these changed since the last frame.
/// Exited state has to be in here too, not just content: a session that dies while its cursor
/// happens to be in its "blink off" phase leaves both `cells` and `cursor` unchanged, so
/// without this the dead-tile border color would never actually get drawn until something else
/// forced a redraw. Cursor blink itself relies on `cursor` here: it toggles `cursor` between
/// `Some`/`None` each phase (see `TerminalSession::snapshot`), which is what actually makes the
/// blink visible frame-to-frame.
type FrameKey = (
    String,
    Vec<(usize, usize, char, Option<(u8, u8, u8)>)>,
    Option<(usize, usize, terminal::CursorKind)>,
    Vec<(usize, usize)>,
    Vec<(usize, usize, (u8, u8, u8))>,
    bool,
);
// Standard-ish terminal cursor blink rate (iTerm2/Terminal.app are both in this ballpark).
const BLINK_INTERVAL: Duration = Duration::from_millis(530);
// Gap between reconnecting saved sessions on startup (see `pending_reconnects`'s doc comment)
// — heavy shell configs (Powerlevel10k's instant-prompt, rbenv/nvm init, etc.) can race with
// themselves when several copies start at the exact same instant; spreading the spawns out
// avoids that without meaningfully slowing down how fast the app feels ready to use.
const RECONNECT_STAGGER: Duration = Duration::from_millis(1500);
// Must match the `left`/`top` origin used in `RedrawRequested`'s call to `gpu.text.render` for
// each tile — duplicated here (rather than computed once and stored) because it's cheap and
// keeps the margin tweakable in one place without a stale-cache field to remember to update.
const TEXT_LEFT_MARGIN: f64 = 8.0;
const TEXT_TOP_MARGIN: f64 = 4.0;
// Logical px; scaled to physical px the same way border thickness is. Clamped per-tile against
// half the tile's own width/height (see `render_rounded_corners`) so this stays sane once
// enough sessions are open that tiles shrink well below this.
const TILE_CORNER_RADIUS: f64 = 10.0;
// A left press that stays within `LONG_PRESS_SLOP` logical px of where it started for this long
// picks the tile up for a drag-to-swap (see `App.press_pending` and the `MouseInput`/
// `about_to_wait` handlers) instead of being a text-selection drag. Moving further than the slop
// before the delay elapses commits the press to a text selection.
const LONG_PRESS_DELAY: Duration = Duration::from_millis(300);
const LONG_PRESS_SLOP: f64 = 4.0;
// How long a tile-swap slide takes to move the two tiles into their new positions — short
// enough not to feel sluggish, long enough to read as a move rather than a jump.
const TILE_ANIM: Duration = Duration::from_millis(160);

/// An in-progress tile-swap slide (see `App.tile_anim`). `from` is where each tile — indexed in
/// the *post-swap* `App.terms` order — sat just before the swap; `RedrawRequested` eases every
/// tile's drawn rect from `from[i]` to its final `tile_rects` position over `TILE_ANIM`.
struct TileAnim {
    start: Instant,
    from: Vec<(f64, f64, f64, f64)>,
}

/// Logical-space (x, y, width, height) rectangles — one per currently-open session, in the
/// same order as `App.terms` — tiling them across the window's terminal area (everything
/// right of the sidebar) in a roughly-square grid (`ceil(sqrt(n))` columns), the classic
/// tiling-window-manager layout the plan doc's Phase 3 calls for. Recomputed on demand rather
/// than cached, since it only depends on cheap inputs (window size, session count).
fn tile_rects(window: &Window, n: usize, right_inset: f64) -> Vec<(f64, f64, f64, f64)> {
    if n == 0 {
        return Vec::new();
    }
    let scale = window.scale_factor();
    let size = window.inner_size();
    let area_w = (size.width as f64 / scale - SIDEBAR_WIDTH - right_inset).max(1.0);
    let area_h = (size.height as f64 / scale).max(1.0);
    let cols = (n as f64).sqrt().ceil() as usize;
    let rows = (n + cols - 1) / cols;
    let tile_w = area_w / cols as f64;
    let tile_h = area_h / rows as f64;
    (0..n)
        .map(|i| {
            let col = (i % cols) as f64;
            let row = (i / cols) as f64;
            (SIDEBAR_WIDTH + col * tile_w, row * tile_h, tile_w, tile_h)
        })
        .collect()
}

/// Index into `tile_rects`' output of the tile containing a logical-space point, if any
/// (`None` means the point is over the sidebar or outside every tile, e.g. a partial last row).
fn tile_at(rects: &[(f64, f64, f64, f64)], logical_x: f64, logical_y: f64) -> Option<usize> {
    rects.iter().position(|&(x, y, w, h)| {
        logical_x >= x && logical_x < x + w && logical_y >= y && logical_y < y + h
    })
}

/// Forces a child webview's NSView back to the front of its superview's subview stack — the
/// wgpu Metal surface otherwise composites over it (see the long comment at the original
/// sidebar-webview creation for the full why, and the linked wgpu/wry issues). No-op off macOS.
fn raise_child_webview(webview: &WebView) {
    #[cfg(target_os = "macos")]
    {
        use wry::WebViewExtMacOS;
        let wk_webview = webview.webview();
        // wry pins a different `objc2` than this crate, so `Retained<T>` isn't a shared type —
        // reinterpret the underlying Objective-C `id` (same object either way) as this crate's
        // own `NSView` binding via a raw-pointer cast.
        let ns_view: &objc2_app_kit::NSView =
            unsafe { &*((&*wk_webview) as *const _ as *const objc2_app_kit::NSView) };
        if let Some(superview) = unsafe { ns_view.superview() } {
            unsafe {
                superview.addSubview_positioned_relativeTo(
                    ns_view,
                    objc2_app_kit::NSWindowOrderingMode::NSWindowAbove,
                    None,
                );
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = webview;
}

/// How many columns/rows of terminal grid fit in a `width_px` x `height_px` (physical pixels)
/// area, given the render margins — used both for the initial per-tile terminal spawn and to
/// keep each session's grid (and its pty's own idea of its size, via `TerminalSession::
/// resize`) in sync with its tile on every resize/session-count change.
///
/// `cell_w` is the *measured* glyph advance width (see `TextPipeline::measure_cell_width` —
/// logical px, same units as `terminal::CELL_H`), not a guessed constant: a guess even
/// slightly smaller than the font's true width overestimates how many columns fit, which
/// showed up as shell prompts rendering past their tile's true edge and getting clipped away.
fn grid_size_for_area(scale: f64, width_px: f64, height_px: f64, cell_w: f64) -> (usize, usize) {
    let left = TEXT_LEFT_MARGIN * scale;
    let top = TEXT_TOP_MARGIN * scale;
    let cell_w = cell_w * scale;
    let cell_h = terminal::CELL_H as f64 * scale;
    let cols = ((width_px - left) / cell_w).floor().max(1.0) as usize;
    let rows = ((height_px - top) / cell_h).floor().max(1.0) as usize;
    (cols, rows)
}

/// Maps a window-relative physical-pixel point to a display-space terminal cell (column, row)
/// *within a specific tile* (`tile_x`/`tile_y` are that tile's logical-space origin from
/// `tile_rects`) — used for mouse selection. `cell_w` — see `grid_size_for_area`'s doc comment.
fn point_to_cell_in_tile(
    scale: f64,
    tile_x: f64,
    tile_y: f64,
    physical_x: f64,
    physical_y: f64,
    cell_w: f64,
) -> (usize, i32) {
    let left = tile_x * scale + TEXT_LEFT_MARGIN * scale;
    let top = tile_y * scale + TEXT_TOP_MARGIN * scale;
    let cell_w = cell_w * scale;
    let cell_h = terminal::CELL_H as f64 * scale;
    let col = ((physical_x - left) / cell_w).floor().max(0.0) as usize;
    let row = ((physical_y - top) / cell_h).floor().max(0.0) as i32;
    (col, row)
}

/// Maps a plain (unmodified-by-Control) character to the C0 control byte a terminal expects for
/// Ctrl+that-character, e.g. `b` (Ctrl+B) → 0x02. Matches the standard VT100-derived convention
/// every terminal follows (`byte = uppercase(c) - 'A' + 1` for letters, plus a handful of
/// punctuation keys), not something specific to this app. Shared by `macos_input_view.rs`'s
/// Ctrl-combo handling and `window_event`'s non-macOS `KeyboardInput` handling below — the same
/// mapping regardless of which platform's input path produced the keystroke.
fn control_byte(c: char) -> Option<u8> {
    match c.to_ascii_uppercase() {
        'A'..='Z' => Some(c.to_ascii_uppercase() as u8 - b'A' + 1),
        '[' => Some(0x1B), // same byte as plain Escape
        '\\' => Some(0x1C),
        ']' => Some(0x1D),
        '^' => Some(0x1E),
        '_' => Some(0x1F),
        '?' => Some(0x7F), // same byte as Backspace/DEL
        _ => None,
    }
}

/// Custom winit user event, delivered on the main thread from background threads —
/// `IpcResponse` from the IPC dispatch threads (ipc.rs), `PtyOutput` from a terminal's pty
/// reader thread (terminal.rs) so redraws are driven by actual new output instead of a
/// blind timer (which was re-shaping the whole grid ~60x/sec regardless of whether anything
/// had changed, pegging the CPU and starving the sidebar webview's own rendering).
pub enum AppEvent {
    IpcResponse(String),
    // Carries which session produced output, so `user_event` can record it in `App.activity`
    // (Phase 4's sidebar activity dot) as well as trigger a redraw.
    PtyOutput(String),
    // Sent by `macos_input_view::TerminalInputView` (Phase 1d — see the plan doc) instead of
    // winit's own `WindowEvent::Ime`/`KeyboardInput`, which had a confirmed macOS-only bug:
    // its internal IME state machine could silently drop the keystroke immediately following
    // a composition commit (e.g. the space after a Vietnamese Telex word). The custom view
    // owns keyboard input entirely on macOS now, so these carry what it decided instead, and
    // always target whichever session is currently `active_id` (the tile last clicked into).
    ImePreedit(String),
    ImeCommit(String),
    KeyControl(&'static str),
    // Ctrl+letter (Ctrl+C to interrupt, Ctrl+D for EOF, Ctrl+Z to suspend, shell readline
    // shortcuts, etc.) — see `macos_input_view::control_byte`'s doc comment for why these need
    // their own path instead of going through `KeyControl`/AppKit's key-binding table.
    KeyByte(u8),
    // Sent by `TerminalInputView::doCommandBySelector`/`keyDown:` when it sees Cmd+C/Cmd+V —
    // actual clipboard I/O happens here in `user_event` since the view doesn't have access to
    // `TerminalSession`.
    Copy,
    Paste,
    // Sent by `ipc.rs` after the sidebar's `create_session`/`close_session`/session-click
    // commands successfully touch the database — the *live* pty-backed session (Phase 3:
    // multi-session tiling) is owned entirely here in `App`, not reachable from the IPC
    // dispatch background threads, so it's created/destroyed/focused in response to these.
    // `ssh_password` is `Some` only for `ipc.rs`'s `connect_ssh` handler, when the credential's
    // auth method is password-based — stashed into `App::pending_ssh_passwords` on spawn so the
    // next `PtyOutput` for this session can type it in the moment `ssh`'s own prompt appears
    // (see that field's doc comment). Every other spawn path leaves this `None`.
    SpawnSession {
        id: String,
        cwd: String,
        shell: String,
        shell_args: Vec<String>,
        ssh_password: Option<String>,
    },
    CloseSession { id: String },
    FocusSession(String),
    // Sent by `ipc.rs`'s `send_to_session` command — the sidebar's per-session "Resume Claude"
    // button (sends `claude --continue\r`), rather than something the currently-focused tile's
    // own keyboard input already covers. Unlike keyboard/paste events (which always target
    // `active_id`), this names its session explicitly since the sidebar can trigger it for any
    // session, not just the focused one.
    SendToSession { id: String, text: String },
    // Sent by `control.rs`'s `run` command — types `payload` (a marker-wrapped one-liner) into
    // the `to_id` session's pty and arms an output capture keyed on `nonce`, so a Claude Code
    // agent (or the `termhub-msg run` CLI) in another session can run a command in this one and
    // get its stdout + exit code back. `reply` is answered by the target's pty reader thread
    // once the end marker lands (or the capture times out); `control.rs` blocks on it.
    RunInSession {
        to_id: String,
        nonce: String,
        payload: String,
        reply: std::sync::mpsc::Sender<terminal::CaptureOutcome>,
    },
    // Sent by a session's pty reader thread (terminal.rs) once its `read()` loop ends — the
    // shell process is gone. Phase 5: marks the tile dead in `App.exited` instead of leaving
    // its last frame frozen on screen with no visual difference from a live idle session.
    SessionExited(String),
    // Sent by `ipc.rs`'s `set_overlay_open` command whenever any full-window modal (usage
    // dashboard, settings) opens or closes — widens/narrows the sidebar webview to match (see
    // `App.webview_full`'s doc comment for why a modal needs this instead of the webview just
    // being full-window-sized always). The frontend is responsible for only reporting "closed"
    // once *every* modal it owns is closed, since this is a single shared flag, not a count.
    SetOverlayOpen(bool),
    // Sent by `ipc.rs`'s `toggle_message_log` command (the chat-button rail / the panel's × in
    // `MessageLog.tsx`) — expands or collapses the right-docked message-log panel between its
    // rail and full width (`App.log_panel`), reflowing the tiles to the new terminal-area width.
    ToggleMessageLog,
    // Sent by `control.rs` whenever an inter-session message is delivered — forwarded into the
    // log-panel webview as a `termhub:message` DOM event so the feed updates live.
    MessageLogged {
        from_id: Option<String>,
        from_name: Option<String>,
        to_id: Option<String>,
        to_name: String,
        body: String,
        ts: i64,
    },
    // Sent by `control.rs` once per recipient of a delivered message — drives the sidebar's
    // arrival toast (`termhub:message-toast`). Separate from `MessageLogged` (one per send, for
    // the transcript panel) because it's per-target and carries the recipient's session id.
    MessageNudge { to_id: String, from_name: Option<String>, preview: String },
    // Sent by `control.rs` instead of `MessageNudge` when `intersession_autodeliver` is on and
    // the target isn't mid-`inbox --wait` — types the message straight into the target
    // session's pty (bracketed paste + Enter) so a keyboard-driven agent there picks it up as a
    // submitted prompt.
    MessageInject { to_id: String, from_name: Option<String>, body: String },
    // Sent by `ipc.rs`'s `set_accent_color` command when the user picks a different accent
    // color in Settings — updates `App.accent_color` (see its doc comment) so the native
    // active-tile border repaints with it immediately, without needing the app restarted to
    // pick up what's now saved in the db. Already-validated `#rrggbb` RGB floats, not the raw
    // hex string — `commands::set_accent_color` parses and rejects a malformed value before
    // this is ever sent.
    SetAccentColor([f32; 3]),
    // Sent by `macos_input_view::key_down` for the new/close/next/prev-session shortcuts
    // (Cmd+T/Cmd+W/Cmd+Shift+]/Cmd+Shift+[). Session bookkeeping (the `sessions` list,
    // `activeId`) lives in the sidebar's React state, not here in `App`, so this is forwarded
    // into the webview as a DOM event rather than handled natively — same reasoning as why
    // `SpawnSession`/`CloseSession` originate from IPC commands, just in the opposite
    // direction. One of "new-session"/"close-session"/"next-session"/"prev-session".
    KeyboardShortcut(&'static str),
    // Sent by `macos_input_view::TerminalInputView`'s `flagsChanged:` the moment right Option is
    // physically pressed down — push-to-talk, matching how a physical walkie-talkie button
    // works: starts dictating into whichever session is `active_id`. See that method's doc
    // comment for why a bare modifier key (not a Cmd combo — an earlier version of this tried
    // Cmd+M) is the only reliable way to do this on macOS. `VoiceInputStop` (sent by the same
    // handler once right Option comes back up) is what actually ends it. Routed through `App`
    // (rather than handled by the sidebar's own React state, unlike the session-management
    // shortcuts above) because recording is native mic/engine state that only `App` has any
    // business owning — see `speech.rs`'s module doc comment.
    #[cfg(target_os = "macos")]
    VoiceInputStart,
    // Sent by `flagsChanged:` when right Option comes back up — ends whatever recording
    // `VoiceInputStart` began. A no-op if nothing is currently recording (shouldn't normally
    // happen for a single physical key, but harmless either way).
    #[cfg(target_os = "macos")]
    VoiceInputStop,
    // Sent by `speech::start` when it had to kick off the system's speech-recognition
    // permission prompt instead of starting to record immediately (first use only) — carries
    // whether the user granted it, so `App` knows whether to retry starting the recording it was
    // asked for or give up and tell the user why.
    #[cfg(target_os = "macos")]
    VoiceAuthResult(bool),
    // Sent every time `speech.rs`'s recognition result handler fires with a non-empty result —
    // possibly many times per recording as the recognized text is refined. `is_final` marks the
    // one call that represents the complete, settled transcript for this recording (arrives
    // after `speech::stop` ends the audio, once the recognizer finishes processing whatever was
    // still buffered) — only that one actually gets written into the terminal; every earlier one
    // is just shown live as a preview (reusing the same preedit overlay IME composition uses).
    #[cfg(target_os = "macos")]
    VoiceTranscript { text: String, is_final: bool },
    // Sent once a recording is fully done one way or another: normally right after the final
    // `VoiceTranscript` (itself following a `VoiceInputStop`), but also on its own if the
    // recognizer errored out before ever producing a final result (network hiccup, Apple's
    // ~1-minute recognition cap, denied mid-flight, etc.) — the `Some(message)` case is the only
    // path that can end a recording without a matching `VoiceInputStop` from the user, so `App`
    // has to treat this, not just the key release, as the authority on whether it's still
    // holding a live `VoiceSession`.
    #[cfg(target_os = "macos")]
    VoiceEnded(Option<String>),
    // Sent by `ipc.rs`'s `set_voice_ptt_keycode` command when the user picks a different
    // push-to-talk key in Settings — forwarded straight to the live `TerminalInputView` (see
    // `TerminalInputView::set_ptt_keycode`) so the change takes effect immediately, without
    // needing the app restarted to pick up what's now saved in the db.
    #[cfg(target_os = "macos")]
    SetVoicePttKeycode(u16),
    // Sent by `ipc.rs`'s `set_shortcut`/`reset_shortcut` commands whenever a native keyboard
    // shortcut (Copy, Paste, New/Close/Next/Prev session, Open folder) is changed in Settings —
    // the complete, current set of effective bindings (db overrides merged with built-in
    // defaults — see `commands::get_shortcuts`), forwarded to the live `TerminalInputView` (see
    // `TerminalInputView::set_shortcuts`) so the change takes effect immediately.
    #[cfg(target_os = "macos")]
    SetShortcuts(Vec<(String, commands::KeyBinding)>),
}

struct GpuState<'a> {
    surface: wgpu::Surface<'a>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    text: TextPipeline,
}

struct App {
    db: Arc<Db>,
    proxy: EventLoopProxy<AppEvent>,
    // Path to the inter-session-messaging control socket (see `control.rs`), injected into
    // every spawned session's env as `TERMHUB_SOCK`. Same value `run()` binds the server on.
    sock_path: std::path::PathBuf,
    window: Option<Arc<Window>>,
    gpu: Option<GpuState<'static>>,
    // `WebViewBuilder::with_ipc_handler`'s closure needs a handle to the webview it's part
    // of constructing — chicken-and-egg — so the closure captures this cell and it's filled
    // in right after `build_as_child` returns. Everything here runs on the main thread, so
    // Rc<RefCell<_>> (not Arc<Mutex<_>>) is enough.
    webview: Rc<RefCell<Option<WebView>>>,
    // Second child webview, docked to the right strip — the inter-session message log
    // (`MessageLog.tsx`, loaded with `?panel=messages`). Always `LOG_PANEL_WIDTH` wide; how much
    // of it sits on screen is `log_panel` (see `LogPanel` / `log_webview_rect`).
    log_webview: Rc<RefCell<Option<WebView>>>,
    log_panel: LogPanel,
    // Every currently-open session's live pty-backed terminal, in tile order (see
    // `tile_rects`). A `Vec` (not a map) because tile layout is order-sensitive and the
    // session count is always small — linear lookup by id is fine at this scale.
    terms: Vec<(String, TerminalSession)>,
    // Which session's tile currently has keyboard focus — `TerminalInputView`'s events (see
    // `AppEvent`) always target this one, and only this tile shows a live cursor/preedit.
    active_id: Option<String>,
    // What was actually drawn last frame, one entry per tile in `terms` order —
    // `RedrawRequested` skips re-shaping/re-rendering a tile whose content and selection
    // haven't changed, since redrawing unconditionally was pegging the CPU (see the plan
    // doc's Phase 1b findings).
    last_frames: Vec<FrameKey>,
    // Session id -> (generation at last snapshot, that snapshot's Frame). `RedrawRequested`
    // used to call `TerminalSession::snapshot` (an O(visible cells) grid walk) for *every*
    // tile on *every* redraw, even tiles whose content hadn't changed — fine while redraws were
    // rare, but scrolling (see `TerminalSession::wheel`'s doc comment on the fractional-pixel
    // fix) now legitimately triggers a redraw on every line crossed, so with several tiles open
    // that meant re-walking every other tile's grid too on each line scrolled in just one of
    // them. Reusing the cached Frame for any inactive tile whose `generation()` hasn't moved
    // skips that work — mirrors `TextPipeline::render_all`'s existing shape-buffer cache. The
    // active tile is always re-snapshotted fresh (never read from here) since only it can show
    // a blinking cursor/IME preedit, neither of which bumps `generation()`.
    frame_cache: HashMap<String, (u64, Frame)>,
    // Session id -> a password-auth "Connect to VPS" credential's saved password, waiting to be
    // typed in the moment `ssh`'s own prompt appears (see `AppEvent::PtyOutput`'s handling,
    // which checks the newly-updated session's `cursor_line_text()` for it) — populated by
    // `AppEvent::SpawnSession`'s `ssh_password`, removed the instant it's used (one-shot: a
    // second, unrelated "password" appearing later in the same session, e.g. the remote shell's
    // own `sudo`, must never get this typed into it too) or the session is closed unused.
    pending_ssh_passwords: HashMap<String, String>,
    // A prompt match just fired (see `AppEvent::PtyOutput`) but the actual pty write is
    // deliberately deferred a few ticks past it rather than done inline on the same event —
    // `ssh` prints the prompt text and *then* finishes switching the pty into its own no-echo
    // read mode; writing the instant the text appears risks racing that setup in a way no real
    // keystroke ever could (a human's reaction time trivially clears it, a same-tick
    // instruction doesn't). `about_to_wait` below fires these once their deadline passes.
    armed_ssh_passwords: Vec<(String, String, Instant)>,
    // `run_in_session`'s two-phase type-in (see `AppEvent::RunInSession`): `stty -echo` is typed
    // immediately, then ~150ms later — drained in `about_to_wait`, once the target's shell has
    // actually turned echo off — the capture is armed and the wrapper typed, so the wrapper
    // line itself is never echoed into the target's scrollback. Entries are
    // (session id, capture nonce, wrapper payload, reply channel, fire-at instant).
    armed_runs: Vec<(String, String, String, std::sync::mpsc::Sender<terminal::CaptureOutcome>, Instant)>,
    // Session ids with a reconnect `SpawnSession` queued by `revive_dead_ssh_sessions` but not
    // yet drained — keeps a second focus/click landing in the same event-loop pass from
    // queueing a duplicate respawn, which would kill the just-revived pty and reconnect twice.
    // Cleared in the `SpawnSession` handler.
    reviving: HashSet<String>,
    cursor_pos: (f64, f64),
    // Live in-progress IME composition text (e.g. "ắ" while still typing Telex, before
    // it's committed) for `active_id`'s session — not sent to the pty, just overlaid at the
    // cursor when rendering so it's visible as you type instead of only appearing once
    // composition ends.
    preedit: String,
    // The custom NSTextInputClient view (Phase 1d) that owns all terminal keyboard/IME input
    // on macOS, replacing winit's own (confirmed buggy) handling entirely. Kept alive here —
    // dropping it would tear down the AppKit view it wraps.
    #[cfg(target_os = "macos")]
    input_view: Option<objc2::rc::Retained<macos_input_view::TerminalInputView>>,
    // Non-macOS terminal keyboard/IME input (Phase 5) goes through winit's own
    // `WindowEvent::KeyboardInput`/`Ime` instead of a custom view — winit's own handling was
    // only disabled for macOS due to its confirmed IME bug (see `AppEvent::ImePreedit`'s doc
    // comment), so this reuses it as-is rather than reimplementing a second custom input path.
    // `KeyboardInput` events don't carry modifier state directly; it has to be tracked
    // separately from `WindowEvent::ModifiersChanged` and consulted here.
    #[cfg(not(target_os = "macos"))]
    modifiers: winit::keyboard::ModifiersState,
    // Mouse-drag text selection (Phase 2). `selecting_tile` holds the id of the session whose
    // selection is being extended — fixed at mouse-down, not re-resolved as the cursor moves,
    // so dragging past a tile's edge keeps extending that tile's selection rather than
    // switching tiles mid-drag.
    selecting_tile: Option<String>,
    // A left press on a tile that hasn't yet resolved into a text selection (cursor moved past
    // `LONG_PRESS_SLOP`) or a tile pick-up (held still for `LONG_PRESS_DELAY`, promoted in
    // `about_to_wait`). Holds (session id, press instant, press cursor pos in physical px).
    press_pending: Option<(String, Instant, (f64, f64))>,
    // Tile drag-to-reorder: set once a press is held long enough to pick the tile up (see
    // `press_pending`). Holds the id of the session being dragged; on mouse-up the tile under
    // the cursor swaps grid positions with it. `tile_rects` is driven purely by `terms` order,
    // so a `terms.swap` is the whole move.
    tile_drag: Option<String>,
    // Set on a completed pick-up-drag swap; drives the slide animation in `RedrawRequested` and
    // is cleared there once `TILE_ANIM` has elapsed. See `TileAnim`.
    tile_anim: Option<TileAnim>,
    // Leftover fractional pixel-scroll distance carried between `MouseWheel` events. A single
    // trackpad callback's `PixelDelta` is often just a few px — smaller than `CELL_H` — so
    // converting it to whole lines and discarding the remainder every time made slow, deliberate
    // scrolling (e.g. reading back through a long pasted prompt) feel unresponsive; only fast
    // flicks ever crossed a full line. Accumulating here lets several small events add up to a
    // line instead of each being individually rounded away.
    scroll_remainder: f64,
    // Cursor blink phase, toggled on a timer in `about_to_wait` — see `TerminalSession::
    // snapshot`'s doc comment for why a small periodic wakeup here is fine even though this
    // app is otherwise fully event-driven.
    cursor_visible: bool,
    next_blink: Instant,
    // Measured once in `resumed()` via `TextPipeline::measure_cell_width` — see
    // `grid_size_for_area`'s doc comment for why this can't just be a guessed constant.
    // Defaults to `terminal::CELL_W` only as a placeholder before the window (and the
    // `TextPipeline`/font system needed to actually measure it) exists.
    cell_w: f64,
    // Saved sessions still waiting to be reconnected at startup, spawned one at a time on a
    // timer in `about_to_wait` (`RECONNECT_STAGGER` apart) instead of all at once in
    // `resumed()`. Spawning N heavy shells in the exact same instant is a real problem with a
    // heavy `.zshrc` (Powerlevel10k's instant-prompt feature in particular has a known failure
    // mode where concurrent shell startups race on its cache file and produce corrupted
    // prompt output — confirmed via this app's own reproduction: content that should have
    // been a normal `~ ... ok HH:MM:SS PM` prompt line came out as garbled/partial text, and
    // heavy simultaneous startups were consistent with the app eventually becoming
    // unresponsive) — staggering the spawns removes the "several copies starting at the exact
    // same nanosecond" trigger for that race entirely.
    pending_reconnects: std::collections::VecDeque<session::SessionMeta>,
    next_reconnect: Instant,
    // Session id -> last pty-output timestamp, shared with `ipc.rs`'s `get_activity` command
    // (Phase 4's sidebar activity dot) — see `Activity`'s doc comment.
    activity: Activity,
    // Ids of sessions whose shell process has exited, shared with `ipc.rs`'s
    // `get_exited_sessions` command (Phase 5) — see `Exited`'s doc comment. The tile stays in
    // `terms` (so its scrollback is still visible) but is rendered dead until respawned.
    exited: Exited,
    // See `ActiveSession`'s doc comment — written once at the end of `resumed()`, read once by
    // `ipc.rs`'s `get_active_session`.
    active: ActiveSession,
    // Whether the sidebar webview should currently cover the *whole* window instead of just
    // the `SIDEBAR_WIDTH` strip — true while the usage dashboard modal is open. The modal is
    // React content rendered inside that same webview with CSS expecting to center itself over
    // a full-window viewport (`position: fixed; inset: 0`), but the webview is normally kept
    // narrow on purpose (see the mouse-click guard in `window_event`'s `MouseInput` handler)
    // so clicks past the sidebar fall through to the native terminal tiles instead of being
    // captured by the child webview. Left narrow, the modal only had a 220px-wide viewport to
    // center itself in and rendered clipped against that edge instead of over the app.
    // Widening the webview only while the modal is actually open keeps the click-passthrough
    // behavior intact the rest of the time.
    webview_full: bool,
    // The active-tile border color (`terminal.rs`'s `render_tile_border`), shared with the
    // sidebar's own `--accent-color` CSS variable so both sides of the UI agree — set once at
    // startup from whatever's saved in the db (`commands::get_accent_color`, falling back to
    // `commands::DEFAULT_ACCENT_COLOR`), then live-updated by `AppEvent::SetAccentColor` when
    // the user picks a different one in Settings. Kept as plain RGB floats (not a hex string)
    // since that's what `render_tile_border` needs every frame — parsed once here rather than
    // re-parsed per frame.
    accent_color: [f32; 3],
    // The live mic/recognizer state for an in-progress dictation (push-to-talk — `AppEvent::
    // VoiceInputStart`/`VoiceInputStop`). `Some` only while audio is actually being captured —
    // `stop_voice` takes this (calling `speech::stop`) the moment the key is released, which is
    // *before* the final transcript has actually arrived, so this alone can't be used to route
    // that final result; see `voice_target` for that.
    #[cfg(target_os = "macos")]
    voice: Option<speech::VoiceSession>,
    // Which session the current (or just-stopped-but-still-finishing) dictation targets —
    // distinct from `voice` because it has to outlive `speech::stop` taking `voice`: the
    // recognizer keeps delivering results asynchronously for a bit after capture stops, and the
    // final one (`AppEvent::VoiceTranscript { is_final: true, .. }`) needs to land in the same
    // tile dictation started in even if focus moved elsewhere in between — not just whatever tile
    // happens to be `active_id` when that final result shows up. Cleared only once the whole
    // recognition lifecycle ends (`AppEvent::VoiceEnded`).
    #[cfg(target_os = "macos")]
    voice_target: Option<String>,
}

impl App {
    fn new(
        db: Arc<Db>,
        proxy: EventLoopProxy<AppEvent>,
        activity: Activity,
        exited: Exited,
        active: ActiveSession,
    ) -> Self {
        let accent_color = commands::get_accent_color(&db)
            .ok()
            .flatten()
            .and_then(|hex| commands::parse_hex_color(&hex))
            .unwrap_or_else(|| {
                commands::parse_hex_color(commands::DEFAULT_ACCENT_COLOR)
                    .expect("DEFAULT_ACCENT_COLOR is a valid hex color")
            });
        Self {
            db,
            proxy,
            sock_path: control_socket_path(),
            window: None,
            gpu: None,
            webview: Rc::new(RefCell::new(None)),
            log_webview: Rc::new(RefCell::new(None)),
            log_panel: LogPanel::Hidden,
            terms: Vec::new(),
            active_id: None,
            last_frames: Vec::new(),
            frame_cache: HashMap::new(),
            pending_ssh_passwords: HashMap::new(),
            armed_ssh_passwords: Vec::new(),
            armed_runs: Vec::new(),
            reviving: HashSet::new(),
            cursor_pos: (0.0, 0.0),
            preedit: String::new(),
            #[cfg(target_os = "macos")]
            input_view: None,
            #[cfg(not(target_os = "macos"))]
            modifiers: winit::keyboard::ModifiersState::empty(),
            selecting_tile: None,
            press_pending: None,
            tile_drag: None,
            tile_anim: None,
            scroll_remainder: 0.0,
            cursor_visible: true,
            next_blink: Instant::now() + BLINK_INTERVAL,
            cell_w: terminal::CELL_W as f64,
            pending_reconnects: std::collections::VecDeque::new(),
            next_reconnect: Instant::now(),
            activity,
            exited,
            active,
            webview_full: false,
            accent_color,
            #[cfg(target_os = "macos")]
            voice: None,
            #[cfg(target_os = "macos")]
            voice_target: None,
        }
    }

    /// Resets the cursor to solid/visible and pushes its next blink-off deadline out — called
    /// on every keystroke, matching the usual terminal UX where typing while the cursor
    /// happens to be in its "off" blink phase doesn't make it look unresponsive.
    fn reset_blink(&mut self) {
        self.cursor_visible = true;
        self.next_blink = Instant::now() + BLINK_INTERVAL;
    }

    fn is_exited(&self, id: &str) -> bool {
        self.exited.lock().map(|s| s.contains(id)).unwrap_or(false)
    }

    /// If the currently-focused tile's session has exited, respawns it in place (same id, same
    /// cwd) instead of writing to its dead pty — lets a click or keystroke on a dead tile
    /// revive it without going through the sidebar's close-then-reopen. Returns whether it did,
    /// so callers know to skip whatever pty write/selection they were about to do; the
    /// keystroke or click that triggered the revive is swallowed rather than also forwarded to
    /// the freshly spawned shell.
    fn respawn_active_if_exited(&mut self) -> bool {
        let Some(id) = self.active_id.clone() else { return false };
        if !self.is_exited(&id) {
            return false;
        }
        if let Ok(meta) = self.db.get_session(&id) {
            let ssh_password = commands::ssh_reconnect_password(&self.db, &meta);
            let _ = self.proxy.send_event(AppEvent::SpawnSession {
                id,
                cwd: meta.cwd,
                shell: meta.shell,
                shell_args: meta.shell_args,
                ssh_password,
            });
        }
        true
    }

    /// Reconnects every dead SSH-backed tile in place (same id, same cwd). A dropped connection
    /// — "broken pipe" — exits the remote shell, so the tile is marked dead in `App.exited` and
    /// its last frame frozen; without this the operator has to click each dead VPS tile one by
    /// one to bring it back. Called from the focus paths (window regaining focus, switching to
    /// or clicking any session) so focusing anywhere revives them all at once.
    ///
    /// Only SSH sessions (those with a saved `ssh_credential_id`) — a local shell the user
    /// `exit`ed must not respawn behind their back. `skip_active` is set by the click/focus
    /// paths, where `respawn_active_if_exited` has already handled the focused tile (and revives
    /// a dead local tile there too, matching the existing click-to-revive).
    ///
    /// Goes through `AppEvent::SpawnSession` (like `respawn_active_if_exited`), which sets
    /// `active_id` to each tile it revives — so a trailing `FocusSession` restores whatever the
    /// user actually had focused. `reviving` guards against a second call in the same event-loop
    /// pass queueing the same respawn twice.
    fn revive_dead_ssh_sessions(&mut self, skip_active: bool) {
        let restore = self.active_id.clone();
        let dead: Vec<String> = self
            .terms
            .iter()
            .map(|(id, _)| id.clone())
            .filter(|id| !(skip_active && Some(id) == restore.as_ref()))
            .filter(|id| self.is_exited(id))
            .filter(|id| !self.reviving.contains(id))
            .filter(|id| {
                self.db
                    .get_session(id)
                    .map(|m| m.ssh_credential_id.is_some())
                    .unwrap_or(false)
            })
            .collect();
        if dead.is_empty() {
            return;
        }
        for id in &dead {
            let Ok(meta) = self.db.get_session(id) else { continue };
            let ssh_password = commands::ssh_reconnect_password(&self.db, &meta);
            self.reviving.insert(id.clone());
            let _ = self.proxy.send_event(AppEvent::SpawnSession {
                id: id.clone(),
                cwd: meta.cwd,
                shell: meta.shell,
                shell_args: meta.shell_args,
                ssh_password,
            });
        }
        if let Some(id) = restore {
            let _ = self.proxy.send_event(AppEvent::FocusSession(id));
        }
    }

    fn active_term(&mut self) -> Option<&mut TerminalSession> {
        let id = self.active_id.clone()?;
        self.terms.iter_mut().find(|(tid, _)| *tid == id).map(|(_, t)| t)
    }

    /// Handles `AppEvent::VoiceInputStart` (right Option pressed down) — starts dictating
    /// into the focused tile, and re-enters itself (see `AppEvent::VoiceAuthResult`) once a
    /// first-use permission prompt resolves. A no-op if a recording is already in progress
    /// (shouldn't normally happen for a single physical key, but harmless either way: this only
    /// ever starts capture, never restarts it out from under an existing session).
    /// Committing the recognized text happens later, in `user_event`'s `AppEvent::
    /// VoiceTranscript` handling, once the final transcript actually arrives.
    #[cfg(target_os = "macos")]
    fn start_voice(&mut self) {
        if self.voice.is_some() {
            return;
        }
        let Some(id) = self.active_id.clone() else { return };
        if self.is_exited(&id) {
            return;
        }
        match speech::start(self.proxy.clone()) {
            Ok(Some(session)) => {
                self.voice = Some(session);
                self.voice_target = Some(id);
                self.reset_blink();
                self.report_voice_state(true);
            }
            // Authorization prompt is up; `AppEvent::VoiceAuthResult` calls back into this once
            // the user answers it. By the time that lands right Option has often already been
            // released (answering the system dialog takes a mouse click), so there's nothing to
            // reconcile here — just start capture as if the key were still held; a stray
            // `VoiceInputStop` will end it immediately if it really was released, and if the
            // user's still holding it this just starts capture like normal.
            Ok(None) => {}
            Err(message) => self.report_voice_error(message),
        }
    }

    /// Handles `AppEvent::VoiceInputStop` (right Option released) — ends the current
    /// recording if there is one. A no-op otherwise (see `AppEvent::VoiceInputStop`'s doc
    /// comment for why that's expected, not just defensive).
    #[cfg(target_os = "macos")]
    fn stop_voice(&mut self) {
        let Some(session) = self.voice.take() else { return };
        speech::stop(session);
        self.preedit.clear();
        self.report_voice_state(false);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Surfaces a dictation failure to the user via the same sidebar toast/banner path other
    /// native errors in this app use, rather than only ever logging to stderr where nobody
    /// running the packaged `.app` would ever see it.
    #[cfg(target_os = "macos")]
    fn report_voice_error(&self, message: String) {
        if let Some(wv) = self.webview.borrow().as_ref() {
            let payload = serde_json::to_string(&message).unwrap_or_else(|_| "\"\"".to_string());
            let script = format!(
                "window.dispatchEvent(new CustomEvent('termhub:voice-error', {{ detail: {payload} }}))"
            );
            let _ = wv.evaluate_script(&script);
        }
    }

    /// Tells the sidebar whether a dictation is currently in progress, so it can light up its
    /// one mic icon — pushed immediately on every start/stop rather than polled (unlike the
    /// activity dot/exited badge) since push-to-talk needs to feel instant, not laggy by
    /// however long a poll interval would add.
    #[cfg(target_os = "macos")]
    fn report_voice_state(&self, recording: bool) {
        if let Some(wv) = self.webview.borrow().as_ref() {
            let script = format!(
                "window.dispatchEvent(new CustomEvent('termhub:voice-state', {{ detail: {recording} }}))"
            );
            let _ = wv.evaluate_script(&script);
        }
    }

    /// Tells the sidebar which tile actually has focus, for whenever `active_id` changes on the
    /// Rust side without the sidebar driving it (a tile clicked directly, or the fallback pick
    /// after closing the active session) — see the `MouseInput` handler and
    /// `AppEvent::CloseSession` below. Sidebar-driven changes (`AppEvent::FocusSession`, sidebar
    /// clicks) don't need this: the frontend already set its own `activeId` before ever asking
    /// Rust. `id` is always a `Uuid::new_v4()` string (`commands::create_session`) or `None`,
    /// never user input, so this doesn't need JSON-escaping.
    fn report_active_session(&self, id: Option<&str>) {
        if let Some(wv) = self.webview.borrow().as_ref() {
            let detail = match id {
                Some(id) => format!("'{id}'"),
                None => "null".to_string(),
            };
            let script = format!(
                "window.dispatchEvent(new CustomEvent('termhub:session-focused', {{ detail: {detail} }}))"
            );
            let _ = wv.evaluate_script(&script);
        }
    }

    /// Tells the frontend to close any open overlay (usage dashboard, settings, quick-open) —
    /// fired both when the window loses focus (see `WindowEvent::Focused`) and when Escape is
    /// pressed while one is open (see the `KeyControl`/non-macOS `KeyboardInput` handling
    /// below), since neither the terminal's native keyboard focus nor window-focus state has
    /// any way to know an overlay is open on top of it.
    fn dismiss_overlays(&self) {
        if let Some(wv) = self.webview.borrow().as_ref() {
            let _ = wv
                .evaluate_script("window.dispatchEvent(new CustomEvent('termhub:close-overlays'))");
        }
    }

    /// The sidebar webview's bounds for the current window size — the narrow `SIDEBAR_WIDTH`
    /// strip normally, or the full window while `webview_full` is set (see its doc comment).
    /// Shared by initial webview creation, the resize handler, and the usage-overlay toggle so
    /// all three agree on what "full" and "narrow" mean.
    fn webview_rect(&self, window: &Window) -> Rect {
        let scale = window.scale_factor();
        let size = window.inner_size();
        let width = if self.webview_full { size.width as f64 / scale } else { SIDEBAR_WIDTH };
        // Explicit physical pixels, not `Logical*` — the sidebar was rendering as only a thin
        // sliver of its intended `SIDEBAR_WIDTH`, cut off well short of its actual React
        // layout (confirmed correct independently: the same page loaded in a plain browser
        // renders the full-width panel fine), which pointed at wry applying the logical→
        // physical conversion inconsistently with `winit`'s own `scale_factor()` on this
        // build. Passing already-scaled physical values sidesteps that conversion entirely.
        Rect {
            position: PhysicalPosition::new(0.0, 0.0).into(),
            size: PhysicalSize::new(width * scale, size.height as f64).into(),
        }
    }

    /// How much horizontal space the message-log webview currently claims from the terminal
    /// area's right edge — the full panel, just the chat-button rail, or nothing.
    fn log_inset(&self) -> f64 {
        match self.log_panel {
            LogPanel::Hidden => 0.0,
            LogPanel::Collapsed => LOG_HANDLE_WIDTH,
            LogPanel::Open => LOG_PANEL_WIDTH,
        }
    }

    /// The log webview's bounds — always `LOG_PANEL_WIDTH` wide (no 0-size child-view edge case,
    /// and the React layout never reflows on a state change), positioned so exactly `log_inset()`
    /// of it shows on the right. Physical px for the same reason `webview_rect` uses them.
    fn log_webview_rect(&self, window: &Window) -> Rect {
        let scale = window.scale_factor();
        let size = window.inner_size();
        let w = LOG_PANEL_WIDTH * scale;
        let x = size.width as f64 - self.log_inset() * scale;
        Rect {
            position: PhysicalPosition::new(x, 0.0).into(),
            size: PhysicalSize::new(w, size.height as f64).into(),
        }
    }

    /// Re-applies both child webviews' bounds for the current window size / `webview_full` /
    /// `log_panel` state — called from every place that changes one of those.
    fn sync_webview_bounds(&self, window: &Window) {
        if let Some(wv) = self.webview.borrow().as_ref() {
            let _ = wv.set_bounds(self.webview_rect(window));
        }
        if let Some(wv) = self.log_webview.borrow().as_ref() {
            let _ = wv.set_bounds(self.log_webview_rect(window));
        }
    }

    /// Re-lay-out and repaint after `log_panel` changed, and tell both webviews the new state
    /// (`termhub:log-state` detail is `"hidden"` | `"collapsed"` | `"open"`).
    fn refresh_log_panel(&mut self) {
        let Some(window) = self.window.clone() else { return };
        self.sync_webview_bounds(&window);
        self.refit_all_tiles(&window); // terminal area grew/shrank by the panel delta
        self.last_frames.clear();
        window.request_redraw();
        let state = match self.log_panel {
            LogPanel::Hidden => "hidden",
            LogPanel::Collapsed => "collapsed",
            LogPanel::Open => "open",
        };
        let script = format!(
            "window.dispatchEvent(new CustomEvent('termhub:log-state', {{ detail: '{state}' }}))"
        );
        if let Some(wv) = self.webview.borrow().as_ref() {
            let _ = wv.evaluate_script(&script);
        }
        if let Some(wv) = self.log_webview.borrow().as_ref() {
            let _ = wv.evaluate_script(&script);
        }
    }

    /// Refits every open session's grid (and its pty's `winsize`) to its current tile — called
    /// after the window resizes or the number of open sessions changes, either of which
    /// changes every tile's size via `tile_rects`.
    fn refit_all_tiles(&mut self, window: &Window) {
        let scale = window.scale_factor();
        let rects = tile_rects(window, self.terms.len(), self.log_inset());
        for ((_, term), &(_, _, w, h)) in self.terms.iter_mut().zip(rects.iter()) {
            let (cols, rows) = grid_size_for_area(scale, w * scale, h * scale, self.cell_w);
            term.resize(cols, rows);
        }
    }

    /// Spawns one new session sized for the tile it will end up in (once it's added to the
    /// grid), then resizes every other already-open tile to fit the new total — shared by
    /// live session creation (`AppEvent::SpawnSession`) and the staggered startup reconnect
    /// (`pending_reconnects`). Returns whether the spawn succeeded.
    fn spawn_session(
        &mut self,
        window: &Window,
        id: String,
        cwd: &str,
        shell: &str,
        shell_args: &[String],
    ) -> bool {
        let scale = window.scale_factor();
        let rects = tile_rects(window, self.terms.len() + 1, self.log_inset());
        let &(_, _, w, h) = rects.last().unwrap_or(&(0.0, 0.0, 0.0, 0.0));
        let (cols, rows) = grid_size_for_area(scale, w * scale, h * scale, self.cell_w);
        // Session's display name, for the `TERMHUB_SESSION_NAME` env var (inter-session
        // messaging). Empty if the row's somehow already gone — not worth failing the spawn.
        let name = self.db.get_session(&id).map(|m| m.name).unwrap_or_default();
        // Fresh per-pty auth token for `TERMHUB_TOKEN` (see `control.rs`'s inbox check). A
        // failed write just means the token check falls back to lenient for this session.
        let token = uuid::Uuid::new_v4().to_string();
        let _ = self.db.set_session_token(&id, &token);
        match TerminalSession::spawn(
            id.clone(),
            cwd,
            shell,
            shell_args,
            &name,
            &self.sock_path,
            &token,
            cols,
            rows,
            self.proxy.clone(),
        ) {
            Ok(term) => {
                self.terms.push((id, term));
                self.refit_all_tiles(window);
                true
            }
            Err(_) => false,
        }
    }
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("TermHub")
            .with_inner_size(winit::dpi::LogicalSize::new(1100.0, 700.0));
        let window = Arc::new(event_loop.create_window(attrs).unwrap());
        // On macOS the custom `TerminalInputView` (installed below) owns IME entirely —
        // winit's own handling is left disabled so its separate, confirmed-buggy IME state
        // machine never activates in parallel. Non-macOS platforms don't have that view yet
        // (Phase 1d is macOS-only so far), so they still need winit's own IME support.
        #[cfg(not(target_os = "macos"))]
        window.set_ime_allowed(true);

        let size = window.inner_size();

        // --- wgpu terminal surface, fills the whole window; the sidebar webview docks on
        // top of the left strip of it via native child-view compositing ---
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone()).unwrap();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("no suitable GPU adapter found");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("failed to create wgpu device");
        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats[0];
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        let mut text = TextPipeline::new(&device, &queue, format, config.width, config.height);
        self.cell_w = text.measure_cell_width() as f64;

        // --- two child webviews sharing the window with the wgpu terminal surface: the sidebar
        // docked to the left strip, and the message-log panel parked off the right edge until
        // toggled open. Both load the same bundle; `?panel=messages` tells the frontend's
        // `main.tsx` to mount `MessageLog` instead of `App`. ---
        let (webview, log_webview) = {
            let build_panel = |bounds: Rect, query: &'static str| -> WebView {
                let db = self.db.clone();
                let proxy = self.proxy.clone();
                let activity = self.activity.clone();
                let exited = self.exited.clone();
                let active = self.active.clone();
                let builder = WebViewBuilder::new().with_bounds(bounds).with_transparent(true);
                #[cfg(debug_assertions)]
                let builder = builder.with_url(format!("{}/{query}", dev_server_url()));
                #[cfg(not(debug_assertions))]
                let builder = builder
                    .with_custom_protocol("termhub".into(), |_id, request| {
                        let path = request.uri().path().trim_start_matches('/');
                        let path = if path.is_empty() { "index.html" } else { path };
                        match assets::Assets::get(path) {
                            Some(file) => wry::http::Response::builder()
                                .header("Content-Type", assets::mime_of(path))
                                .body(std::borrow::Cow::from(file.data.into_owned()))
                                .unwrap(),
                            None => wry::http::Response::builder()
                                .status(404)
                                .body(std::borrow::Cow::from(Vec::new()))
                                .unwrap(),
                        }
                    })
                    .with_url(format!("termhub://localhost/index.html{query}"));
                builder
                    .with_ipc_handler(move |msg| {
                        ipc::spawn_dispatch(
                            db.clone(),
                            activity.clone(),
                            exited.clone(),
                            active.clone(),
                            proxy.clone(),
                            msg.body(),
                        );
                    })
                    .build_as_child(&*window)
                    .expect("failed to build child webview")
            };
            (
                build_panel(self.webview_rect(&window), ""),
                build_panel(self.log_webview_rect(&window), "?panel=messages"),
            )
        };
        // Raise the log panel first, then the sidebar — sidebar ends up frontmost so a
        // full-window overlay modal (`webview_full`) always covers the log panel too.
        raise_child_webview(&log_webview);
        raise_child_webview(&webview);
        *self.webview.borrow_mut() = Some(webview);
        *self.log_webview.borrow_mut() = Some(log_webview);

        // --- reconnect a live pty-backed terminal for every session already saved in the db
        // (Phase 3: multi-session tiling — previously this spawned exactly one hardcoded
        // session regardless of what was in the sidebar) ---
        // Drop sessions whose cwd no longer exists (e.g. a folder deleted since it was saved)
        // *before* computing tile rects, not after trying to spawn them — the rects are sized
        // for however many sessions will actually end up live, so any spawn failure discovered
        // mid-loop would otherwise leave every later tile's initial pty size mismatched with
        // its actual on-screen tile (each session's shell reads its column count once at
        // startup, so a wrong initial size shows up as a garbled first prompt, not something
        // that self-corrects on the resize that follows).
        let mut metas: std::collections::VecDeque<_> = self
            .db
            .list_sessions()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| std::path::Path::new(&m.cwd).is_dir())
            .collect();
        // Nothing to reconnect — either a genuinely fresh install, or every saved session's
        // folder got filtered out above, or the "Reopen previous sessions?" prompt in `run()`
        // was answered "No" (which clears the table outright). Whatever the reason, landing on
        // a window with zero tiles and no on-screen way to open one (the sidebar's "new
        // session" button is the only other entry point, and only takes effect once a session
        // already exists to click into) is a dead end — spawn one default session so there's
        // always at least something live to look at.
        if metas.is_empty() {
            if let Ok(info) = commands::create_session(&self.db, None, None) {
                metas.push_back(info.meta);
            }
        }
        if let Some(first) = metas.pop_front() {
            let ssh_password = commands::ssh_reconnect_password(&self.db, &first);
            let id = first.id.clone();
            if self.spawn_session(&window, first.id, &first.cwd, &first.shell, &first.shell_args) {
                if let Some(password) = ssh_password {
                    self.pending_ssh_passwords.insert(id, password);
                }
            }
        }
        self.pending_reconnects = metas;
        self.next_reconnect = Instant::now() + RECONNECT_STAGGER;
        self.active_id = self.terms.first().map(|(id, _)| id.clone());
        // Seeds `ipc.rs`'s `get_active_session` for the frontend's initial mount fetch — see
        // `ActiveSession`'s doc comment for why this is a one-shot pull instead of a push.
        if let Ok(mut active) = self.active.lock() {
            *active = self.active_id.clone();
        }

        // Install the custom NSTextInputClient view and hand it first responder immediately
        // so terminal keyboard input goes through it (and its correct IME handling) from the
        // start, not through winit's own (disabled above) machinery.
        #[cfg(target_os = "macos")]
        {
            let ptt_keycode = commands::get_voice_ptt_keycode(&self.db)
                .ok()
                .flatten()
                .unwrap_or(macos_input_view::DEFAULT_PTT_KEYCODE);
            let shortcuts = commands::get_shortcuts(&self.db)
                .map(|list| list.into_iter().map(|(id, _, binding)| (id, binding)).collect())
                .unwrap_or_default();
            self.input_view =
                macos::install_input_view(&window, self.proxy.clone(), ptt_keycode, shortcuts);
        }

        self.gpu = Some(GpuState { surface, device, queue, config, text });
        self.window = Some(window);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::IpcResponse(script) => {
                if let Some(wv) = self.webview.borrow().as_ref() {
                    let _ = wv.evaluate_script(&script);
                }
            }
            AppEvent::PtyOutput(id) => {
                // SSH auto-password: if this session is still waiting to have one typed in,
                // check whether the line its cursor is now sitting on looks like `ssh`'s own
                // password prompt (`"user@host's password:"`, or a retry's plain
                // `"password:"`) — a substring match is deliberately loose rather than trying
                // to parse the exact OpenSSH wording, which varies (first prompt vs. a retry
                // after a wrong password) and isn't worth pinning down exactly. One-shot: acted
                // on at most once per session, so a *different* later "password" prompt (the
                // remote shell's own `sudo`, say) is never typed into.
                if let Some(password) = self.pending_ssh_passwords.get(&id).cloned() {
                    let matched = self
                        .terms
                        .iter()
                        .find(|(tid, _)| *tid == id)
                        .is_some_and(|(_, term)| term.cursor_line_text().to_lowercase().contains("password"));
                    if matched {
                        self.pending_ssh_passwords.remove(&id);
                        self.armed_ssh_passwords.push((
                            id.clone(),
                            password,
                            Instant::now() + Duration::from_millis(150),
                        ));
                    }
                }
                if let Ok(mut activity) = self.activity.lock() {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    activity.insert(id, now_ms);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            AppEvent::ImePreedit(text) => {
                self.reset_blink();
                self.preedit = text;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            AppEvent::ImeCommit(text) => {
                self.reset_blink();
                self.preedit.clear();
                // A keystroke landing on a dead tile revives it instead of writing into its
                // dead pty (Phase 5) — the committed text is swallowed rather than also handed
                // to the freshly spawned shell.
                if !self.respawn_active_if_exited() {
                    if let Some(term) = self.active_term() {
                        term.write(&text);
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            AppEvent::KeyControl(seq) => {
                // Escape closes an open overlay instead of reaching the terminal — the custom
                // `TerminalInputView` (macOS) always turns Escape into this event regardless of
                // whether an overlay is covering the screen, since it has no notion of the
                // webview's own state.
                if seq == "\x1b" && self.webview_full {
                    self.dismiss_overlays();
                    return;
                }
                self.reset_blink();
                if !self.respawn_active_if_exited() {
                    if let Some(term) = self.active_term() {
                        term.write(seq);
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            AppEvent::KeyByte(byte) => {
                self.reset_blink();
                if !self.respawn_active_if_exited() {
                    if let Some(term) = self.active_term() {
                        // `byte` is always < 0x80 (a C0 control code), so it's trivially valid
                        // single-byte UTF-8 on its own.
                        term.write(&(byte as char).to_string());
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            AppEvent::Copy => {
                let Some(term) = self.active_term() else { return };
                if let Some(text) = term.selection_to_string() {
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        let _ = clipboard.set_text(text);
                    }
                }
            }
            AppEvent::Paste => {
                self.reset_blink();
                // A copied *file* (e.g. Cmd-C on a screenshot in Finder) must be handled before
                // falling through to `get_image`: see `macos::clipboard_file_url`'s doc comment
                // — Finder also declares an Icon-Services placeholder bitmap under the image
                // pasteboard types, which `get_image` can't tell apart from a real copied
                // bitmap, so checking the file-URL type first and using the file's own path
                // (rather than re-deriving image bytes from the pasteboard at all) sidesteps
                // that placeholder entirely.
                #[cfg(target_os = "macos")]
                let file_path = crate::macos::clipboard_file_url();
                #[cfg(not(target_os = "macos"))]
                let file_path: Option<std::path::PathBuf> = None;

                let pasted = if let Some(path) = file_path {
                    Some(format!("'{}'", path.display()))
                } else {
                    let Ok(mut clipboard) = arboard::Clipboard::new() else { return };
                    // A screenshot/copied image has no meaningful text representation to paste
                    // into a pty — matching iTerm2/Warp/VS Code's terminal, write it to a temp
                    // file and paste the path instead, so it can be handed to a CLI tool that
                    // takes a file argument. `get_image` is tried first since a lot of image
                    // sources (e.g. macOS screenshot-to-clipboard) don't also populate a text
                    // representation for `get_text` to fall back on.
                    match clipboard.get_image() {
                        Ok(img) => save_clipboard_image(&img),
                        Err(_) => clipboard.get_text().ok(),
                    }
                };
                if let Some(text) = pasted {
                    if let Some(term) = self.active_term() {
                        term.paste(&text);
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            #[cfg(target_os = "macos")]
            AppEvent::VoiceInputStart => self.start_voice(),
            #[cfg(target_os = "macos")]
            AppEvent::VoiceInputStop => self.stop_voice(),
            #[cfg(target_os = "macos")]
            AppEvent::VoiceAuthResult(authorized) => {
                // Authorization was requested mid-`start_voice` (first use only) — nothing was
                // recording yet in that call, so if it's now granted, try again to actually
                // start. If denied, there's nothing recording to clean up either; just surface
                // why to the sidebar.
                if authorized {
                    self.start_voice();
                } else {
                    self.report_voice_error(
                        "Speech recognition access was denied — enable it in System Settings › \
                         Privacy & Security › Speech Recognition to use dictation."
                            .to_string(),
                    );
                }
            }
            #[cfg(target_os = "macos")]
            AppEvent::VoiceTranscript { text, is_final } => {
                let Some(id) = self.voice_target.clone() else { return };
                if is_final {
                    self.preedit.clear();
                    if !text.is_empty() {
                        if let Some((_, term)) = self.terms.iter_mut().find(|(tid, _)| *tid == id) {
                            term.paste(&text);
                        }
                    }
                } else if self.active_id.as_deref() == Some(id.as_str()) {
                    // Only the actively-focused tile renders `preedit` at all (see the
                    // `RedrawRequested` snapshot call below) — updating it while dictating into a
                    // tile that's since lost focus would just be invisible, silently-stale state.
                    self.preedit = text;
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            #[cfg(target_os = "macos")]
            AppEvent::VoiceEnded(error) => {
                // The recognition task itself is done (naturally, or because it errored out).
                // `voice` is normally already `None` here (the user's own `VoiceInputStop`
                // already took it, via `speech::stop`) — the `take()` (and matching
                // `report_voice_state(false)`, since `stop_voice` never ran to send its own)
                // only does real work on the error path, where the recognizer gave up on its
                // own (network hiccup, Apple's ~1-minute recognition cap, etc.) while still
                // actively capturing, leaving the engine/tap running with nothing left
                // listening for its output and the sidebar's mic icon stuck lit.
                if let Some(session) = self.voice.take() {
                    speech::stop(session);
                    self.report_voice_state(false);
                }
                self.voice_target = None;
                self.preedit.clear();
                if let Some(message) = error {
                    self.report_voice_error(message);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            #[cfg(target_os = "macos")]
            AppEvent::SetVoicePttKeycode(keycode) => {
                if let Some(view) = &self.input_view {
                    view.set_ptt_keycode(keycode);
                }
            }
            #[cfg(target_os = "macos")]
            AppEvent::SetShortcuts(bindings) => {
                if let Some(view) = &self.input_view {
                    view.set_shortcuts(bindings);
                }
            }
            AppEvent::SpawnSession { id, cwd, shell, shell_args, ssh_password } => {
                let Some(window) = self.window.clone() else { return };
                self.terms.retain(|(tid, _)| *tid != id);
                self.reviving.remove(&id);
                self.pending_ssh_passwords.remove(&id);
                self.armed_ssh_passwords.retain(|(tid, _, _)| *tid != id);
                self.armed_runs.retain(|(tid, ..)| *tid != id);
                // The respawned session starts its own generation counter back at 0 — drop any
                // cached frame for this id so a coincidental generation match against the old
                // (dead) session's last frame can never serve stale content for the new one.
                self.frame_cache.remove(&id);
                // `spawn_session` sizes the new session for the tile it will actually end up
                // in *before* spawning it, rather than spawning at the old (pre-insert)
                // layout's size and correcting afterward — a shell reads its column count once
                // at startup to lay out its first prompt (right-aligned prompt segments, TUIs
                // that query size on launch, etc.), so a spawn-then-resize race left that first
                // prompt rendered for the wrong width, which a later resize doesn't
                // retroactively fix (confirmed: this was the cause of the garbled/overflowing
                // first prompt seen when creating a session while others were already open).
                if !self.spawn_session(&window, id.clone(), &cwd, &shell, &shell_args) {
                    return;
                }
                if let Some(password) = ssh_password {
                    self.pending_ssh_passwords.insert(id.clone(), password);
                }
                // Respawning (whether from the sidebar's "new"/duplicate or reviving a dead
                // tile — Phase 5) always means the tile is alive again.
                if let Ok(mut exited) = self.exited.lock() {
                    exited.remove(&id);
                }
                self.active_id = Some(id);
                #[cfg(target_os = "macos")]
                if let Some(view) = &self.input_view {
                    macos::focus_input_view(view);
                }
                window.request_redraw();
            }
            AppEvent::CloseSession { id } => {
                self.terms.retain(|(tid, _)| *tid != id);
                self.pending_ssh_passwords.remove(&id);
                self.armed_ssh_passwords.retain(|(tid, _, _)| *tid != id);
                self.armed_runs.retain(|(tid, ..)| *tid != id);
                if let Ok(mut exited) = self.exited.lock() {
                    exited.remove(&id);
                }
                if self.active_id.as_deref() == Some(id.as_str()) {
                    self.active_id = self.terms.first().map(|(tid, _)| tid.clone());
                    // The frontend's `handleClose` optimistically sets `activeId` to `null`
                    // when it closes whatever it thinks is active — right whenever there's no
                    // fallback tile, but wrong when another tile picks up focus here instead.
                    self.report_active_session(self.active_id.as_deref());
                }
                if let Some(window) = self.window.clone() {
                    self.refit_all_tiles(&window);
                    window.request_redraw();
                }
            }
            AppEvent::FocusSession(id) => {
                if self.terms.iter().any(|(tid, _)| *tid == id) {
                    self.active_id = Some(id);
                    #[cfg(target_os = "macos")]
                    if let Some(view) = &self.input_view {
                        macos::focus_input_view(view);
                    }
                    // Focusing any session also reconnects every other dead VPS tile.
                    self.revive_dead_ssh_sessions(true);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            AppEvent::SendToSession { id, text } => {
                if let Some((_, term)) = self.terms.iter_mut().find(|(tid, _)| *tid == id) {
                    term.write(&text);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            AppEvent::RunInSession { to_id, nonce, payload, reply } => {
                match self.terms.iter_mut().find(|(tid, _)| *tid == to_id) {
                    Some((_, term)) => {
                        // Phase 1: turn off input echo on its own line, so the wrapper typed in
                        // phase 2 (below, from `about_to_wait` once this has taken effect) never
                        // shows up in the target's scrollback. `2>/dev/null` keeps it quiet on a
                        // shell where `stty` isn't a builtin / the fd isn't a tty.
                        term.write("stty -echo 2>/dev/null\r");
                        self.armed_runs.push((
                            to_id,
                            nonce,
                            payload,
                            reply,
                            Instant::now() + Duration::from_millis(150),
                        ));
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    // Target session isn't live (closed between `list` and `run`) — let the
                    // caller's `recv` unblock immediately instead of waiting out its timeout.
                    None => {
                        let _ = reply.send(terminal::CaptureOutcome::TimedOut);
                    }
                }
            }
            AppEvent::SessionExited(id) => {
                if let Ok(mut exited) = self.exited.lock() {
                    exited.insert(id);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            AppEvent::SetOverlayOpen(open) => {
                self.webview_full = open;
                let Some(window) = self.window.clone() else { return };
                self.sync_webview_bounds(&window);
            }
            AppEvent::ToggleMessageLog => {
                // Driven by the chat-button rail / the panel's × (`MessageLog.tsx`). Only ever
                // fires while the rail is showing, so `Hidden` isn't a real input here.
                self.log_panel = match self.log_panel {
                    LogPanel::Open => LogPanel::Collapsed,
                    _ => LogPanel::Open,
                };
                self.refresh_log_panel();
            }
            AppEvent::MessageLogged { from_id, from_name, to_id, to_name, body, ts } => {
                if let Some(wv) = self.log_webview.borrow().as_ref() {
                    // Resolve each endpoint to its `#N` (1-based creation order — the same
                    // numbering the sidebar and `control.rs` use) here, on the main thread with
                    // the db in hand: the log webview is push-only (`AppEvent::IpcResponse` only
                    // ever targets the sidebar webview), so it can't fetch the session list
                    // itself to map ids to numbers / bubble colours.
                    let sessions = self.db.list_sessions().unwrap_or_default();
                    let num_of = |id: &Option<String>| {
                        id.as_ref()
                            .and_then(|id| sessions.iter().position(|s| &s.id == id))
                            .map(|i| i + 1)
                    };
                    // `body` is arbitrary agent/user text — must be JSON-escaped, unlike the
                    // id/literal payloads the other `evaluate_script` calls here carry.
                    let detail = serde_json::json!({
                        "fromId": from_id,
                        "fromNum": num_of(&from_id),
                        "from": from_name,
                        "toId": to_id,
                        "toNum": num_of(&to_id),
                        "to": to_name,
                        "body": body,
                        "ts": ts,
                    });
                    let _ = wv.evaluate_script(&format!(
                        "window.dispatchEvent(new CustomEvent('termhub:message', {{ detail: {detail} }}))"
                    ));
                }
                // First message this run reveals the chat-button rail (not the full panel — the
                // user expands it themselves). Later messages don't force it open.
                if self.log_panel == LogPanel::Hidden {
                    self.log_panel = LogPanel::Collapsed;
                    self.refresh_log_panel();
                }
            }
            AppEvent::MessageInject { to_id, from_name, body } => {
                if let Some((_, term)) = self.terms.iter_mut().find(|(tid, _)| *tid == to_id) {
                    let who = from_name.as_deref().unwrap_or("an outside shell");
                    // Bracketed paste (see `Terminal::paste`) so a multi-line body and any
                    // pasted-path recognition behave; the trailing CR submits it in an agent
                    // TUI like Claude Code.
                    term.paste(&format!("[termhub-msg from {who}] {body}"));
                    term.write("\r");
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            AppEvent::MessageNudge { to_id, from_name, preview } => {
                // Opt-out via Settings > Messaging (`intersession_toast`, `"0"` = off, absent = on).
                let enabled =
                    self.db.get_setting("intersession_toast").ok().flatten().as_deref() != Some("0");
                if enabled {
                    if let Some(wv) = self.webview.borrow().as_ref() {
                        // `from_name` / `preview` are arbitrary agent text — JSON-escape.
                        let detail = serde_json::json!({
                            "from": from_name,
                            "preview": preview,
                            "toId": to_id,
                        });
                        let _ = wv.evaluate_script(&format!(
                            "window.dispatchEvent(new CustomEvent('termhub:message-toast', {{ detail: {detail} }}))"
                        ));
                    }
                }
            }
            AppEvent::SetAccentColor(rgb) => {
                self.accent_color = rgb;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            AppEvent::KeyboardShortcut(name) => {
                if let Some(wv) = self.webview.borrow().as_ref() {
                    // `name` is always one of this module's own string literals (see the
                    // `AppEvent::KeyboardShortcut` doc comment) — never user input — so this
                    // doesn't need JSON-escaping.
                    let script = format!(
                        "window.dispatchEvent(new CustomEvent('termhub:shortcut', {{ detail: '{name}' }}))"
                    );
                    let _ = wv.evaluate_script(&script);
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x, position.y);
                // Moving beyond the slop before the long-press fires means this press is a
                // text-selection drag, not a tile pick-up — drop the pending pick-up.
                if let Some((_, _, origin)) = &self.press_pending {
                    let scale = self.window.as_ref().map_or(1.0, |w| w.scale_factor());
                    let (dx, dy) = (position.x - origin.0, position.y - origin.1);
                    if (dx * dx + dy * dy).sqrt() > LONG_PRESS_SLOP * scale {
                        self.press_pending = None;
                    }
                }
                if let Some(sel_id) = self.selecting_tile.clone() {
                    if let Some(window) = &self.window {
                        let scale = window.scale_factor();
                        let rects = tile_rects(window, self.terms.len(), self.log_inset());
                        if let Some(idx) = self.terms.iter().position(|(tid, _)| *tid == sel_id) {
                            let (tx, ty, _, _) = rects[idx];
                            let (col, row) =
                                point_to_cell_in_tile(scale, tx, ty, position.x, position.y, self.cell_w);
                            self.terms[idx].1.update_selection(col, row);
                        }
                        window.request_redraw();
                    }
                }
                // A tile pick-up drag in progress — repaint so the drop-target highlight tracks
                // whichever tile the cursor is now over.
                if self.tile_drag.is_some() {
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // The webview child view keeps AppKit "first responder" status even once
                // the window regains key-window focus — those are separate concepts, and
                // `Window::focus_window()` only affects the latter. Explicitly hand first
                // responder back to the custom terminal input view on clicks outside the
                // webview's bounds, or keyboard input stays stuck routing to the webview.
                let Some(window) = self.window.clone() else { return };
                let scale = window.scale_factor();
                let logical_x = self.cursor_pos.0 / scale;
                let logical_y = self.cursor_pos.1 / scale;
                let logical_w = window.inner_size().width as f64 / scale;
                // Ignore presses on the sidebar or on whatever of the message-log webview is
                // currently on screen (its rail and/or full panel — `log_inset()` covers both).
                if logical_x < SIDEBAR_WIDTH || logical_x >= logical_w - self.log_inset() {
                    return;
                }
                #[cfg(target_os = "macos")]
                if let Some(view) = &self.input_view {
                    macos::focus_input_view(view);
                }
                let rects = tile_rects(&window, self.terms.len(), self.log_inset());
                if let Some(idx) = tile_at(&rects, logical_x, logical_y) {
                    let id = self.terms[idx].0.clone();
                    self.active_id = Some(id.clone());
                    self.report_active_session(Some(&id));
                    // Clicking a dead tile (Phase 5) revives it instead of starting a text
                    // selection on its frozen last frame.
                    if !self.respawn_active_if_exited() {
                        let (tx, ty, _, _) = rects[idx];
                        let (col, row) = point_to_cell_in_tile(
                            scale,
                            tx,
                            ty,
                            self.cursor_pos.0,
                            self.cursor_pos.1,
                            self.cell_w,
                        );
                        let (_, term) = &mut self.terms[idx];
                        term.clear_selection();
                        term.start_selection(col, row);
                        self.selecting_tile = Some(id.clone());
                        // Same press also arms a tile pick-up: if it stays put for
                        // `LONG_PRESS_DELAY` (checked in `about_to_wait`), the tentative
                        // selection above is discarded and the tile enters a drag-to-swap.
                        self.press_pending = Some((id, Instant::now(), self.cursor_pos));
                    }
                    // Clicking any session also reconnects every other dead VPS tile.
                    self.revive_dead_ssh_sessions(true);
                }
                window.request_redraw();
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.selecting_tile = None;
                self.press_pending = None;
                // Finish a pick-up drag: the tile under the cursor swaps grid positions with the
                // dragged one, then both slide into place (see `TileAnim`). A release over the
                // same tile / the sidebar / a gap just clears the drag highlight.
                if let Some(src_id) = self.tile_drag.take() {
                    if let Some(window) = self.window.clone() {
                        let scale = window.scale_factor();
                        let logical_x = self.cursor_pos.0 / scale;
                        let logical_y = self.cursor_pos.1 / scale;
                        let rects = tile_rects(&window, self.terms.len(), self.log_inset());
                        if let (Some(dst), Some(src)) = (
                            tile_at(&rects, logical_x, logical_y),
                            self.terms.iter().position(|(id, _)| *id == src_id),
                        ) {
                            if src != dst {
                                self.terms.swap(src, dst);
                                let mut from = rects.clone();
                                from.swap(src, dst);
                                self.tile_anim = Some(TileAnim { start: Instant::now(), from });
                            }
                        }
                        // Repaint regardless — the drag highlight on the tiles has to clear even
                        // when the drop was a no-op, and `last_frames` is indexed in `terms`
                        // order so it's stale after any swap.
                        self.last_frames.clear();
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Positive `lines` scrolls further back into scrollback history (matches
                // `alacritty_terminal::grid::Scroll::Delta`'s convention — see
                // `TerminalSession::wheel`'s doc comment). Scrolling always targets whichever
                // tile is under the cursor, not necessarily the focused one.
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y.round() as i32,
                    MouseScrollDelta::PixelDelta(pos) => {
                        self.scroll_remainder += pos.y;
                        let lines = (self.scroll_remainder / terminal::CELL_H as f64).trunc() as i32;
                        self.scroll_remainder -= lines as f64 * terminal::CELL_H as f64;
                        lines
                    }
                };
                if lines == 0 {
                    return;
                }
                let Some(window) = self.window.clone() else { return };
                let scale = window.scale_factor();
                let logical_x = self.cursor_pos.0 / scale;
                let logical_y = self.cursor_pos.1 / scale;
                let rects = tile_rects(&window, self.terms.len(), self.log_inset());
                if let Some(idx) = tile_at(&rects, logical_x, logical_y) {
                    let (tx, ty, _, _) = rects[idx];
                    let (col, row) = point_to_cell_in_tile(
                        scale,
                        tx,
                        ty,
                        self.cursor_pos.0,
                        self.cursor_pos.1,
                        self.cell_w,
                    );
                    self.terms[idx].1.wheel(lines, col, row);
                    window.request_redraw();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.config.width = size.width.max(1);
                    gpu.config.height = size.height.max(1);
                    gpu.surface.configure(&gpu.device, &gpu.config);
                    gpu.text.resize(&gpu.queue, gpu.config.width, gpu.config.height);
                }
                if let Some(window) = self.window.clone() {
                    self.sync_webview_bounds(&window);
                    // Keep every tile's grid (and its pty's own `winsize`, so `SIGWINCH`-aware
                    // programs like `vim`/`htop` redraw correctly) in sync with the window —
                    // previously hardcoded to 120x40 regardless of actual window size.
                    self.refit_all_tiles(&window);
                }
            }
            // Dragging the window to a display with a different scale factor (this machine
            // routinely has both: a 1x external monitor and the 2x built-in Retina panel) used
            // to be a complete no-op here — every scale-dependent value this app computes
            // (`window.scale_factor()`, read fresh in a dozen places) only ever gets re-read in
            // response to *some* event actually firing, and this is the one macOS fires for
            // "the window's DPI changed, logical size may be identical" (unlike `Resized`,
            // which fires for a *pixel* size change — the two don't always coincide). Left
            // unhandled, cell measurements, the gpu surface config, and the sidebar webview's
            // bounds all kept whatever scale was correct for the monitor the window was
            // *created* on, silently wrong everywhere after a drag to the other display. Not
            // touching `inner_size_writer` accepts the OS-suggested new size (the default
            // anyway) — this just forces the same reconfiguration `Resized` does, using the
            // fresh scale, immediately rather than hoping a coincidental resize triggers it.
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(window) = self.window.clone() {
                    let size = window.inner_size();
                    if let Some(gpu) = self.gpu.as_mut() {
                        gpu.config.width = size.width.max(1);
                        gpu.config.height = size.height.max(1);
                        gpu.surface.configure(&gpu.device, &gpu.config);
                        gpu.text.resize(&gpu.queue, gpu.config.width, gpu.config.height);
                    }
                    self.sync_webview_bounds(&window);
                    self.refit_all_tiles(&window);
                    window.request_redraw();
                }
            }
            // On macOS, terminal keyboard/IME input no longer flows through winit's own
            // `KeyboardInput`/`Ime` events at all — `TerminalInputView` (Phase 1d) holds
            // first responder and handles it directly, forwarding results via `AppEvent`
            // (see `user_event`). Non-macOS platforms don't have that custom view (Phase 5) —
            // winit's own handling was only ever disabled for macOS specifically, due to its
            // confirmed IME bug, so it's used as-is here instead of a second custom input path.
            #[cfg(not(target_os = "macos"))]
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }
            #[cfg(not(target_os = "macos"))]
            WindowEvent::KeyboardInput { event, .. } => {
                use winit::keyboard::{Key, NamedKey};
                if event.state != ElementState::Pressed {
                    return;
                }
                self.reset_blink();
                // A keystroke landing on a dead tile revives it instead of writing into its
                // dead pty (Phase 5) — same behavior as the macOS input path.
                if self.respawn_active_if_exited() {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Ctrl+letter (interrupt, EOF, readline shortcuts, etc.) needs the raw C0
                // control byte, same as macOS's handling — `event.text` is `None` for these
                // (Ctrl doesn't produce printable text), so this has to come from
                // `logical_key` instead.
                let seq: Option<String> = if self.modifiers.control_key() {
                    match &event.logical_key {
                        Key::Character(s) => {
                            s.chars().next().and_then(control_byte).map(|b| (b as char).to_string())
                        }
                        _ => None,
                    }
                } else {
                    match event.logical_key.as_ref() {
                        Key::Named(NamedKey::Enter) => Some("\r".to_string()),
                        Key::Named(NamedKey::Backspace) => Some("\x7f".to_string()),
                        Key::Named(NamedKey::Tab) => Some("\t".to_string()),
                        Key::Named(NamedKey::Escape) => Some("\x1b".to_string()),
                        Key::Named(NamedKey::ArrowLeft) => Some("\x1b[D".to_string()),
                        Key::Named(NamedKey::ArrowRight) => Some("\x1b[C".to_string()),
                        Key::Named(NamedKey::ArrowUp) => Some("\x1b[A".to_string()),
                        Key::Named(NamedKey::ArrowDown) => Some("\x1b[B".to_string()),
                        Key::Named(NamedKey::Delete) => Some("\x1b[3~".to_string()),
                        Key::Named(NamedKey::Home) => Some("\x1b[H".to_string()),
                        Key::Named(NamedKey::End) => Some("\x1b[F".to_string()),
                        Key::Named(NamedKey::PageUp) => Some("\x1b[5~".to_string()),
                        Key::Named(NamedKey::PageDown) => Some("\x1b[6~".to_string()),
                        // Plain character keys, including anything IME composition already
                        // resolved to final text — dead keys/composing-in-progress states
                        // report `text: None` and are correctly ignored here.
                        _ => event.text.as_ref().map(|s| s.to_string()),
                    }
                };
                if let Some(seq) = seq {
                    // Escape closes an open overlay instead of reaching the terminal — see
                    // `dismiss_overlays`'s doc comment.
                    if seq == "\x1b" && self.webview_full {
                        self.dismiss_overlays();
                    } else if let Some(term) = self.active_term() {
                        term.write(&seq);
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            // Composed text from a system IME (Pinyin, Kana, etc.) — `KeyboardInput` above
            // doesn't fire with real text for keys consumed by an in-progress composition, so
            // this is the only path those come through. Mirrors macOS's
            // `AppEvent::ImePreedit`/`ImeCommit` handling in `user_event`.
            #[cfg(not(target_os = "macos"))]
            WindowEvent::Ime(ime_event) => {
                match ime_event {
                    winit::event::Ime::Preedit(text, _) => {
                        self.reset_blink();
                        self.preedit = text;
                    }
                    winit::event::Ime::Commit(text) => {
                        self.reset_blink();
                        self.preedit.clear();
                        if !self.respawn_active_if_exited() {
                            if let Some(term) = self.active_term() {
                                term.write(&text);
                            }
                        }
                    }
                    winit::event::Ime::Enabled | winit::event::Ime::Disabled => {}
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                // Snapshotted once per frame (not per tile via `self.is_exited`) — `gpu` below
                // is a mutable borrow of `self` for the rest of this arm, which a `&self`
                // method call would conflict with.
                let exited_snapshot: HashSet<String> =
                    self.exited.lock().map(|s| s.clone()).unwrap_or_default();
                // Read before the `gpu` mutable borrow below — `log_inset()` takes `&self`.
                let log_inset = self.log_inset();
                let (Some(gpu), Some(window)) = (self.gpu.as_mut(), self.window.as_ref()) else {
                    return;
                };
                if self.terms.is_empty() {
                    // Closing the last tile must still present a frame — returning here left
                    // whatever was last drawn (the just-closed terminal's content, cursor, and
                    // border) frozen on screen forever, since nothing else ever calls
                    // `surface.present()` again once there are zero tiles to redraw.
                    if !self.last_frames.is_empty() {
                        self.last_frames.clear();
                        let surface_frame = gpu.surface.get_current_texture().unwrap();
                        let view = surface_frame
                            .texture
                            .create_view(&wgpu::TextureViewDescriptor::default());
                        let mut encoder = gpu
                            .device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                        {
                            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: None,
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: &view,
                                    resolve_target: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(wgpu::Color {
                                            r: 0.0,
                                            g: 0.0,
                                            b: 0.0,
                                            a: 1.0,
                                        }),
                                        store: wgpu::StoreOp::Store,
                                    },
                                })],
                                depth_stencil_attachment: None,
                                timestamp_writes: None,
                                occlusion_query_set: None,
                            });
                        }
                        gpu.queue.submit(Some(encoder.finish()));
                        surface_frame.present();
                    }
                    return;
                }
                let scale = window.scale_factor();
                let mut rects = tile_rects(window, self.terms.len(), log_inset);

                // Ease every tile from where it sat pre-swap toward its final slot while a
                // pick-up-drag swap is animating (see `TileAnim`). Only x/y move on a swap —
                // the two tiles are the same size — so w/h are left at their final values.
                let anim_done = if let Some(anim) = &self.tile_anim {
                    let t = (anim.start.elapsed().as_secs_f64() / TILE_ANIM.as_secs_f64())
                        .clamp(0.0, 1.0);
                    if t < 1.0 && anim.from.len() == rects.len() {
                        let e = 1.0 - (1.0 - t).powi(3); // ease-out cubic
                        for (r, &f) in rects.iter_mut().zip(anim.from.iter()) {
                            r.0 = f.0 + (r.0 - f.0) * e;
                            r.1 = f.1 + (r.1 - f.1) * e;
                        }
                    }
                    t >= 1.0
                } else {
                    false
                };
                if anim_done {
                    self.tile_anim = None;
                    // Force this final at-rest frame past the unchanged-content early-return
                    // below so tiles don't stop a fraction of a pixel short.
                    self.last_frames.clear();
                }

                // Drop cached frames for tiles no longer open (closed sessions) so the cache
                // doesn't grow without bound across a long-running app.
                let live_ids: HashSet<&str> = self.terms.iter().map(|(id, _)| id.as_str()).collect();
                self.frame_cache.retain(|id, _| live_ids.contains(id.as_str()));

                let mut frames = Vec::with_capacity(self.terms.len());
                for ((id, term), &(tx, ty, tw, th)) in self.terms.iter().zip(rects.iter()) {
                    // Only actually read inside the `#[cfg(target_os = "macos")]` accessibility
                    // block below — this no-op keeps them from warning as unused on other
                    // platforms without needing to cfg-gate the destructuring pattern itself.
                    let _ = (tx, ty);
                    let is_active = self.active_id.as_deref() == Some(id.as_str());
                    let is_exited = exited_snapshot.contains(id);
                    // Only the focused tile (the one that has keyboard input right now) shows
                    // a cursor and IME preedit — matches the user's own confirmed preference:
                    // each tile is an independent terminal, and the cursor marks which one
                    // you're actually typing into, same as normal window-focus behavior. A
                    // dead tile (Phase 5) never shows a cursor regardless of focus — nothing
                    // is listening on the other end of it to blink for.
                    let preedit = if is_active { self.preedit.as_str() } else { "" };
                    let cursor_visible = is_active && self.cursor_visible && !is_exited;
                    // Active tile always gets a fresh snapshot — it's the only one that can be
                    // showing a blinking cursor or in-progress IME preedit, and neither bumps
                    // `generation()`. Inactive tiles reuse the cached Frame whenever their
                    // generation hasn't moved, skipping the grid walk entirely (see
                    // `frame_cache`'s doc comment).
                    let gen = term.generation();
                    let tframe = if !is_active {
                        match self.frame_cache.get(id) {
                            Some((cached_gen, cached)) if *cached_gen == gen => cached.clone(),
                            _ => {
                                let f = term.snapshot(preedit, cursor_visible);
                                self.frame_cache.insert(id.clone(), (gen, f.clone()));
                                f
                            }
                        }
                    } else {
                        let f = term.snapshot(preedit, cursor_visible);
                        self.frame_cache.insert(id.clone(), (gen, f.clone()));
                        f
                    };
                    frames.push((id.clone(), tframe, (tw, th), is_exited));

                    // Tell the OS where the active tile's text caret actually is on screen —
                    // purely for `NSAccessibility` queries (see `macos_input_view`'s doc
                    // comment on why: a real CLI tool's inline-suggestion popup positions
                    // itself by querying exactly this, and without it there's nothing for
                    // that query to find).
                    #[cfg(target_os = "macos")]
                    if is_active {
                        if let (Some(view), Some((col, row))) =
                            (&self.input_view, term.cursor_position())
                        {
                            let cell_w_px = self.cell_w * scale;
                            let cell_h_px = terminal::CELL_H as f64 * scale;
                            let x = tx * scale + TEXT_LEFT_MARGIN * scale + col as f64 * cell_w_px;
                            let y = ty * scale
                                + TEXT_TOP_MARGIN * scale
                                + row as f64 * cell_h_px;
                            if let Some(rect) =
                                macos::to_screen_rect(window, scale, x, y, cell_w_px, cell_h_px)
                            {
                                view.set_caret_rect(rect);
                                macos::send_cursor_position(rect);
                            }
                        }
                    }
                }

                // Re-shaping every tile's text on every redraw — even when nothing on screen
                // changed anywhere — was pegging the CPU (see the plan doc's Phase 1b
                // findings). Skip the whole frame only when *no* tile's content or selection
                // changed since last time.
                let new_last: Vec<FrameKey> = frames
                    .iter()
                    .map(|(id, f, _, is_exited)| {
                        (
                            id.clone(),
                            f.cells.clone(),
                            f.cursor,
                            f.selection_cells.clone(),
                            f.background_cells.clone(),
                            *is_exited,
                        )
                    })
                    .collect();
                // A live pick-up drag (drop-target highlight follows the cursor) or a running
                // swap slide both need to repaint even though no tile's content changed.
                if new_last == self.last_frames
                    && self.tile_drag.is_none()
                    && self.tile_anim.is_none()
                {
                    return;
                }
                self.last_frames = new_last;

                let surface_frame = gpu.surface.get_current_texture().unwrap();
                let view =
                    surface_frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
                let mut encoder =
                    gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                // iTerm2's default profile: pure black background.
                                load: wgpu::LoadOp::Clear(wgpu::Color {
                                    r: 0.0,
                                    g: 0.0,
                                    b: 0.0,
                                    a: 1.0,
                                }),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });

                    // Every tile's text must be prepared and rendered together in a single
                    // glyphon `prepare`/`render`/`trim` cycle, not one cycle per tile — see
                    // `TextPipeline::render_all`'s doc comment for the real bug that caused
                    // (interleaving `trim()` between tiles evicted glyph data an
                    // already-recorded-but-not-yet-GPU-executed draw call still needed, so
                    // only the last tile processed ever actually showed its text).
                    let cell_w_px = self.cell_w as f32 * scale as f32;
                    let cell_h_px = terminal::CELL_H * scale as f32;
                    let tile_renders: Vec<terminal::TileRender> = frames
                        .iter()
                        .zip(rects.iter())
                        .map(|((_, tframe, _, _), &(tx, ty, tw, th))| {
                            let sx = (tx * scale).round() as i32;
                            let sy = (ty * scale).round() as i32;
                            let sw = (tw * scale).round() as i32;
                            let sh = (th * scale).round() as i32;
                            let tile_left = sx as f32 + TEXT_LEFT_MARGIN as f32 * scale as f32;
                            terminal::TileRender {
                                frame: tframe,
                                left: tile_left,
                                top: sy as f32 + TEXT_TOP_MARGIN as f32 * scale as f32,
                                clip: (sx, sy, sx + sw, sy + sh),
                                cell_w: cell_w_px,
                                cell_h: cell_h_px,
                            }
                        })
                        .collect();
                    gpu.text.render_all(
                        &gpu.device,
                        &gpu.queue,
                        &mut pass,
                        &tile_renders,
                        scale as f32,
                        gpu.config.width,
                        gpu.config.height,
                    );

                    // While a tile is picked up (long-press, see `LONG_PRESS_DELAY`) tint it in
                    // the accent color, and tint whichever tile the cursor is over as the drop
                    // target. Drawn over the tiles' content but before the rounded borders so
                    // each tile's frame still sits on top.
                    if self.tile_drag.is_some() {
                        let drop_idx = self.tile_drag.as_deref().and_then(|src| {
                            let lx = self.cursor_pos.0 / scale;
                            let ly = self.cursor_pos.1 / scale;
                            tile_at(&rects, lx, ly).filter(|&i| frames[i].0 != src)
                        });
                        let mut overlay_rects: Vec<(f32, f32, f32, f32, [f32; 4])> = Vec::new();
                        for (i, ((id, _, _, _), &(tx, ty, tw, th))) in
                            frames.iter().zip(rects.iter()).enumerate()
                        {
                            let a = if self.tile_drag.as_deref() == Some(id.as_str()) {
                                0.18
                            } else if Some(i) == drop_idx {
                                0.10
                            } else {
                                continue;
                            };
                            overlay_rects.push((
                                (tx * scale).round() as f32,
                                (ty * scale).round() as f32,
                                (tw * scale).round() as f32,
                                (th * scale).round() as f32,
                                [self.accent_color[0], self.accent_color[1], self.accent_color[2], a],
                            ));
                        }
                        if !overlay_rects.is_empty() {
                            gpu.text.fill_rects(
                                &gpu.device,
                                &mut pass,
                                &overlay_rects,
                                gpu.config.width,
                                gpu.config.height,
                            );
                        }
                    }

                    // Drawn last — the rounded-corner cleanup this does paints over whatever's
                    // underneath at each corner (see `render_tile_border`'s doc comment), so it
                    // has to run after this tile's own text/background/cursor are already on
                    // screen for this frame, not before like a plain unrounded border could.
                    let radius = (TILE_CORNER_RADIUS * scale) as f32;
                    for ((id, _, (tw, th), is_exited), &(tx, ty, _, _)) in
                        frames.iter().zip(rects.iter())
                    {
                        let sx = (tx * scale).round() as f32;
                        let sy = (ty * scale).round() as f32;
                        let sw = (tw * scale).round() as f32;
                        let sh = (th * scale).round() as f32;
                        let is_active = self.active_id.as_deref() == Some(id.as_str());
                        gpu.text.render_tile_border(
                            &gpu.device,
                            &mut pass,
                            sx,
                            sy,
                            sw,
                            sh,
                            (1.5 * scale as f32).max(1.0),
                            radius,
                            is_active,
                            *is_exited,
                            [self.accent_color[0], self.accent_color[1], self.accent_color[2], 1.0],
                            gpu.config.width,
                            gpu.config.height,
                        );
                    }
                }
                gpu.queue.submit(Some(encoder.finish()));
                surface_frame.present();
            }
            WindowEvent::Focused(focused) => {
                // The window losing focus (Cmd+Tab away, clicking another app, clicking the
                // dock, etc.) should dismiss any open overlay (usage dashboard, settings,
                // quick-open) rather than leaving it stranded on screen once the user's
                // attention — and the webview's own key-window status — has moved elsewhere.
                if !focused {
                    self.dismiss_overlays();
                } else {
                    // Coming back to the app reconnects every dead VPS tile whose SSH
                    // connection dropped while it was in the background ("broken pipe"), so the
                    // operator doesn't have to click each one. Focus stays where it was.
                    self.revive_dead_ssh_sessions(false);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        // Fire any SSH auto-password whose deferred delay (see `armed_ssh_passwords`'s doc
        // comment) has elapsed — split out of the `retain` below since writing needs a mutable
        // borrow of `self.terms`, which `retain`'s own closure can't also hold alongside it.
        let (due, still_waiting): (Vec<_>, Vec<_>) =
            self.armed_ssh_passwords.drain(..).partition(|(_, _, at)| now >= *at);
        self.armed_ssh_passwords = still_waiting;
        for (id, password, _) in due {
            if let Some((_, term)) = self.terms.iter_mut().find(|(tid, _)| *tid == id) {
                term.write(&format!("{password}\r"));
            }
        }
        // Phase 2 of `run_in_session` (see `armed_runs`): the `stty -echo` typed in phase 1 has
        // had its ~150ms to take effect, so arm the capture and type the wrapper now — it lands
        // unechoed.
        let (due_runs, waiting_runs): (Vec<_>, Vec<_>) =
            self.armed_runs.drain(..).partition(|(_, _, _, _, at)| now >= *at);
        self.armed_runs = waiting_runs;
        for (id, nonce, payload, reply, _) in due_runs {
            match self.terms.iter_mut().find(|(tid, _)| *tid == id) {
                Some((_, term)) => {
                    // `control.rs` caps `timeout_secs` at 600 and adds its own recv slack, so
                    // 600s is the ceiling the reader thread needs to enforce.
                    let deadline = now + Duration::from_secs(600);
                    term.begin_capture(&nonce, reply, deadline);
                    // `write`, not `paste`: this goes to a bare shell prompt, so it must land
                    // as typed keystrokes, not a bracketed paste.
                    term.write(&payload);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                // Session closed during the 150ms window — unblock the caller's `recv`.
                None => {
                    let _ = reply.send(terminal::CaptureOutcome::TimedOut);
                }
            }
        }
        if now >= self.next_blink {
            self.cursor_visible = !self.cursor_visible;
            self.next_blink = now + BLINK_INTERVAL;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        if now >= self.next_reconnect {
            if let Some(meta) = self.pending_reconnects.pop_front() {
                let ssh_password = commands::ssh_reconnect_password(&self.db, &meta);
                let id = meta.id.clone();
                if let Some(window) = self.window.clone() {
                    if self.spawn_session(&window, meta.id, &meta.cwd, &meta.shell, &meta.shell_args) {
                        if let Some(password) = ssh_password {
                            self.pending_ssh_passwords.insert(id, password);
                        }
                    }
                    window.request_redraw();
                }
                self.next_reconnect = now + RECONNECT_STAGGER;
            }
        }
        // A press held still on a tile past `LONG_PRESS_DELAY` picks the tile up for a
        // drag-to-swap; the text selection tentatively started on press is discarded.
        if let Some((id, at, _)) = &self.press_pending {
            if now.duration_since(*at) >= LONG_PRESS_DELAY {
                let id = id.clone();
                if let Some(idx) = self.terms.iter().position(|(tid, _)| *tid == id) {
                    self.terms[idx].1.clear_selection();
                }
                self.selecting_tile = None;
                self.tile_drag = Some(id);
                self.press_pending = None;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
        }
        // While a tile-swap slide is running, wake every frame to advance it (see `TileAnim`);
        // `RedrawRequested` clears `tile_anim` once it's done, ending this.
        if self.tile_anim.is_some() {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        // A small periodic wakeup (twice a second) to drive cursor blinking — negligible next
        // to the blind per-frame redraw timer this app deliberately moved away from (see the
        // plan doc's Phase 1b findings); everything else stays purely event-driven. While
        // sessions are still staggering in at startup, also wake for `next_reconnect`.
        let mut deadline = self.next_blink;
        if !self.pending_reconnects.is_empty() {
            deadline = deadline.min(self.next_reconnect);
        }
        if self.tile_anim.is_some() {
            deadline = deadline.min(now + Duration::from_millis(8));
        }
        if let Some((_, at, _)) = &self.press_pending {
            deadline = deadline.min(*at + LONG_PRESS_DELAY);
        }
        if let Some((_, _, at)) = self.armed_ssh_passwords.iter().min_by_key(|(_, _, at)| *at) {
            deadline = deadline.min(*at);
        }
        if let Some(at) = self.armed_runs.iter().map(|(_, _, _, _, at)| *at).min() {
            deadline = deadline.min(at);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
    }
}

/// Writes a clipboard image (raw RGBA8, as `arboard` reads it off the system pasteboard) to a
/// temp PNG file and returns a shell-quoted path to paste into the terminal. Single-quoted
/// (not escaped) since `std::env::temp_dir()` paths on macOS/Linux don't contain single
/// quotes in practice — good enough for this app's scope, not a general shell-quoting utility.
fn save_clipboard_image(img: &arboard::ImageData) -> Option<String> {
    let buffer =
        image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.to_vec())?;
    let path = std::env::temp_dir().join(format!("termhub-paste-{}.png", uuid::Uuid::new_v4()));
    buffer.save(&path).ok()?;
    Some(format!("'{}'", path.display()))
}

#[cfg(debug_assertions)]
fn dev_server_url() -> &'static str {
    "http://localhost:1420"
}

pub(crate) fn app_data_dir() -> std::path::PathBuf {
    // Matches the directory Tauri itself used (`app.path().app_data_dir()` resolves to the
    // same OS convention keyed by the app identifier from the old tauri.conf.json), so the
    // existing termhub.sqlite from before this refactor is found in place.
    dirs::data_dir().expect("no data dir for this platform").join("com.termhub.app")
}

/// Path to the inter-session-messaging control socket (see `control.rs`). Single source of
/// truth: `run()` binds the server here and `App` injects the same path into every session's
/// `TERMHUB_SOCK`.
///
/// Deliberately *not* under the app data dir: a Unix socket path must fit in
/// `sockaddr_un.sun_path` (~104 bytes on macOS), and `~/Library/Application Support/
/// com.termhub.app/` already eats most of that. `/tmp` keeps it short; the username keeps it
/// per-user on a shared machine (same idea as tmux's `/tmp/tmux-<uid>/`).
fn control_socket_path() -> std::path::PathBuf {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "default".into());
    std::path::PathBuf::from(format!("/tmp/termhub-{user}.sock"))
}

pub fn run() {
    std::fs::create_dir_all(app_data_dir()).expect("failed to create app data dir");
    let db = Arc::new(
        Db::open(&app_data_dir().join("termhub.sqlite")).expect("failed to open database"),
    );

    // Ask before silently respawning every saved session on launch — skipped entirely when
    // there's nothing saved (fresh install, nothing to ask about). This runs before the window/
    // event loop exist at all, so a plain blocking native dialog is enough; the actual
    // reconnect-on-launch logic (`App`'s window-creation handler) reads sessions straight from
    // `db` afterward, so clearing it here is sufficient to skip reconnecting — no separate
    // "declined" flag needed anywhere else. Declining discards the saved list outright (same as
    // closing every session individually), not just skipping this one launch, so there's
    // nothing stale left to ask about next time either. `recent_folders` is untouched either
    // way (see `clear_sessions`'s doc comment) — those folders stay reachable from Open Recent.
    if !db.list_sessions().unwrap_or_default().is_empty() {
        let reopen = rfd::MessageDialog::new()
            .set_title("TermHub")
            .set_description("Reopen all previous sessions?")
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if reopen == rfd::MessageDialogResult::No {
            db.clear_sessions().expect("failed to clear saved sessions");
        }
    }

    usage::spawn_tracker(db.clone());

    let mut builder = EventLoop::<AppEvent>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        // Without this, this binary (no .app bundle yet at this stage) doesn't reliably
        // hold keyboard focus on macOS — observed as focus immediately bouncing back off
        // after being granted. See the plan doc's Phase 1a findings.
        builder.with_activation_policy(ActivationPolicy::Regular);
    }
    let event_loop = builder.build().expect("failed to build event loop");
    let proxy = event_loop.create_proxy();
    event_loop.set_control_flow(ControlFlow::Wait);

    // Inter-session messaging control socket (see `control.rs`). Serves on its own background
    // threads; a bind failure is non-fatal (logged, feature disabled). `notify` forwards a
    // delivered message into the event loop so the log-panel webview updates live.
    #[cfg(unix)]
    {
        let notify_proxy = proxy.clone();
        control::spawn(control::ControlState {
            db: db.clone(),
            sock_path: control_socket_path(),
            notify: Box::new(move |ev| {
                let _ = notify_proxy.send_event(ev);
            }),
        });
    }

    let activity: Activity = Arc::new(Mutex::new(HashMap::new()));
    let exited: Exited = Arc::new(Mutex::new(HashSet::new()));
    let active: ActiveSession = Arc::new(Mutex::new(None));
    let mut app = App::new(db, proxy, activity, exited, active);
    event_loop.run_app(&mut app).expect("event loop error");

    // Best-effort: don't leave a dead socket file behind on a clean exit. A hard kill skips
    // this, but `control::spawn` removes any stale socket before it binds on the next launch.
    #[cfg(unix)]
    let _ = std::fs::remove_file(control_socket_path());
}
