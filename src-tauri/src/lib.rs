mod commands;
mod db;
mod ipc;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_input_view;
mod session;
mod terminal;
mod usage;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use db::Db;
use terminal::{TerminalSession, TextPipeline};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

const SIDEBAR_WIDTH: f64 = 220.0;

/// Session id -> unix-epoch milliseconds of its last pty output — shared (not just an `App`
/// field) because the sidebar's `get_activity` IPC command (Phase 4's activity dot) reads it
/// from a background dispatch thread (`ipc.rs`), while `App::user_event` writes it on the main
/// thread whenever `AppEvent::PtyOutput` arrives.
type Activity = Arc<Mutex<HashMap<String, u64>>>;

/// One tile's change-detection key for `App.last_frames` — id, text spans (with color),
/// selected cells, and background-colored cells. `RedrawRequested` skips a tile entirely when
/// none of these changed since the last frame.
type FrameKey = (
    String,
    Vec<(String, Option<(u8, u8, u8)>)>,
    Vec<(usize, usize)>,
    Vec<(usize, usize, (u8, u8, u8))>,
);
// Standard-ish terminal cursor blink rate (iTerm2/Terminal.app are both in this ballpark).
const BLINK_INTERVAL: Duration = Duration::from_millis(530);
// Gap between reconnecting saved sessions on startup (see `pending_reconnects`'s doc comment)
// — heavy shell configs (Powerlevel10k's instant-prompt, rbenv/nvm init, etc.) can race with
// themselves when several copies start at the exact same instant; spreading the spawns out
// avoids that without meaningfully slowing down how fast the app feels ready to use.
const RECONNECT_STAGGER: Duration = Duration::from_millis(700);
// Must match the `left`/`top` origin used in `RedrawRequested`'s call to `gpu.text.render` for
// each tile — duplicated here (rather than computed once and stored) because it's cheap and
// keeps the margin tweakable in one place without a stale-cache field to remember to update.
const TEXT_LEFT_MARGIN: f64 = 8.0;
const TEXT_TOP_MARGIN: f64 = 4.0;

/// Logical-space (x, y, width, height) rectangles — one per currently-open session, in the
/// same order as `App.terms` — tiling them across the window's terminal area (everything
/// right of the sidebar) in a roughly-square grid (`ceil(sqrt(n))` columns), the classic
/// tiling-window-manager layout the plan doc's Phase 3 calls for. Recomputed on demand rather
/// than cached, since it only depends on cheap inputs (window size, session count).
fn tile_rects(window: &Window, n: usize) -> Vec<(f64, f64, f64, f64)> {
    if n == 0 {
        return Vec::new();
    }
    let scale = window.scale_factor();
    let size = window.inner_size();
    let area_w = (size.width as f64 / scale - SIDEBAR_WIDTH).max(1.0);
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
    SpawnSession { id: String, cwd: String },
    CloseSession { id: String },
    FocusSession(String),
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
    window: Option<Arc<Window>>,
    gpu: Option<GpuState<'static>>,
    // `WebViewBuilder::with_ipc_handler`'s closure needs a handle to the webview it's part
    // of constructing — chicken-and-egg — so the closure captures this cell and it's filled
    // in right after `build_as_child` returns. Everything here runs on the main thread, so
    // Rc<RefCell<_>> (not Arc<Mutex<_>>) is enough.
    webview: Rc<RefCell<Option<WebView>>>,
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
    // Mouse-drag text selection (Phase 2). `selecting_tile` holds the id of the session whose
    // selection is being extended — fixed at mouse-down, not re-resolved as the cursor moves,
    // so dragging past a tile's edge keeps extending that tile's selection rather than
    // switching tiles mid-drag.
    selecting_tile: Option<String>,
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
}

impl App {
    fn new(db: Arc<Db>, proxy: EventLoopProxy<AppEvent>, activity: Activity) -> Self {
        Self {
            db,
            proxy,
            window: None,
            gpu: None,
            webview: Rc::new(RefCell::new(None)),
            terms: Vec::new(),
            active_id: None,
            last_frames: Vec::new(),
            cursor_pos: (0.0, 0.0),
            preedit: String::new(),
            #[cfg(target_os = "macos")]
            input_view: None,
            selecting_tile: None,
            cursor_visible: true,
            next_blink: Instant::now() + BLINK_INTERVAL,
            cell_w: terminal::CELL_W as f64,
            pending_reconnects: std::collections::VecDeque::new(),
            next_reconnect: Instant::now(),
            activity,
        }
    }

    /// Resets the cursor to solid/visible and pushes its next blink-off deadline out — called
    /// on every keystroke, matching the usual terminal UX where typing while the cursor
    /// happens to be in its "off" blink phase doesn't make it look unresponsive.
    fn reset_blink(&mut self) {
        self.cursor_visible = true;
        self.next_blink = Instant::now() + BLINK_INTERVAL;
    }

    fn active_term(&mut self) -> Option<&mut TerminalSession> {
        let id = self.active_id.clone()?;
        self.terms.iter_mut().find(|(tid, _)| *tid == id).map(|(_, t)| t)
    }

    /// Refits every open session's grid (and its pty's `winsize`) to its current tile — called
    /// after the window resizes or the number of open sessions changes, either of which
    /// changes every tile's size via `tile_rects`.
    fn refit_all_tiles(&mut self, window: &Window) {
        let scale = window.scale_factor();
        let rects = tile_rects(window, self.terms.len());
        for ((_, term), &(_, _, w, h)) in self.terms.iter_mut().zip(rects.iter()) {
            let (cols, rows) = grid_size_for_area(scale, w * scale, h * scale, self.cell_w);
            term.resize(cols, rows);
        }
    }

    /// Spawns one new session sized for the tile it will end up in (once it's added to the
    /// grid), then resizes every other already-open tile to fit the new total — shared by
    /// live session creation (`AppEvent::SpawnSession`) and the staggered startup reconnect
    /// (`pending_reconnects`). Returns whether the spawn succeeded.
    fn spawn_session(&mut self, window: &Window, id: String, cwd: &str) -> bool {
        let scale = window.scale_factor();
        let rects = tile_rects(window, self.terms.len() + 1);
        let &(_, _, w, h) = rects.last().unwrap_or(&(0.0, 0.0, 0.0, 0.0));
        let (cols, rows) = grid_size_for_area(scale, w * scale, h * scale, self.cell_w);
        match TerminalSession::spawn(id.clone(), cwd, cols, rows, self.proxy.clone()) {
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
        let scale = window.scale_factor();

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

        // --- sidebar webview docked to the left strip, replacing the old Tauri-owned window ---
        let rect = Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: LogicalSize::new(SIDEBAR_WIDTH, size.height as f64 / scale).into(),
        };
        let db_for_ipc = self.db.clone();
        let proxy_for_ipc = self.proxy.clone();
        let activity_for_ipc = self.activity.clone();
        let webview = WebViewBuilder::new()
            .with_bounds(rect)
            .with_url(dev_server_url())
            .with_ipc_handler(move |msg| {
                ipc::spawn_dispatch(
                    db_for_ipc.clone(),
                    activity_for_ipc.clone(),
                    proxy_for_ipc.clone(),
                    msg.body(),
                );
            })
            .build_as_child(&*window)
            .expect("failed to build sidebar webview");
        *self.webview.borrow_mut() = Some(webview);

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
        if let Some(first) = metas.pop_front() {
            self.spawn_session(&window, first.id, &first.cwd);
        }
        self.pending_reconnects = metas;
        self.next_reconnect = Instant::now() + RECONNECT_STAGGER;
        self.active_id = self.terms.first().map(|(id, _)| id.clone());

        // Install the custom NSTextInputClient view and hand it first responder immediately
        // so terminal keyboard input goes through it (and its correct IME handling) from the
        // start, not through winit's own (disabled above) machinery.
        #[cfg(target_os = "macos")]
        {
            self.input_view = macos::install_input_view(&window, self.proxy.clone());
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
                if let Some(term) = self.active_term() {
                    term.write(&text);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            AppEvent::KeyControl(seq) => {
                self.reset_blink();
                if let Some(term) = self.active_term() {
                    term.write(seq);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            AppEvent::KeyByte(byte) => {
                self.reset_blink();
                if let Some(term) = self.active_term() {
                    // `byte` is always < 0x80 (a C0 control code), so it's trivially valid
                    // single-byte UTF-8 on its own.
                    term.write(&(byte as char).to_string());
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
                let Ok(mut clipboard) = arboard::Clipboard::new() else { return };
                // A screenshot/copied image has no meaningful text representation to paste
                // into a pty — matching iTerm2/Warp/VS Code's terminal, write it to a temp
                // file and paste the path instead, so it can be handed to a CLI tool that
                // takes a file argument. `get_image` is tried first since a lot of image
                // sources (e.g. macOS screenshot-to-clipboard) don't also populate a text
                // representation for `get_text` to fall back on.
                let pasted = match clipboard.get_image() {
                    Ok(img) => save_clipboard_image(&img),
                    Err(_) => clipboard.get_text().ok(),
                };
                if let Some(text) = pasted {
                    if let Some(term) = self.active_term() {
                        term.write(&text);
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            AppEvent::SpawnSession { id, cwd } => {
                let Some(window) = self.window.clone() else { return };
                self.terms.retain(|(tid, _)| *tid != id);
                // `spawn_session` sizes the new session for the tile it will actually end up
                // in *before* spawning it, rather than spawning at the old (pre-insert)
                // layout's size and correcting afterward — a shell reads its column count once
                // at startup to lay out its first prompt (right-aligned prompt segments, TUIs
                // that query size on launch, etc.), so a spawn-then-resize race left that first
                // prompt rendered for the wrong width, which a later resize doesn't
                // retroactively fix (confirmed: this was the cause of the garbled/overflowing
                // first prompt seen when creating a session while others were already open).
                if !self.spawn_session(&window, id.clone(), &cwd) {
                    return;
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
                if self.active_id.as_deref() == Some(id.as_str()) {
                    self.active_id = self.terms.first().map(|(tid, _)| tid.clone());
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
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x, position.y);
                if let Some(sel_id) = self.selecting_tile.clone() {
                    if let Some(window) = &self.window {
                        let scale = window.scale_factor();
                        let rects = tile_rects(window, self.terms.len());
                        if let Some(idx) = self.terms.iter().position(|(tid, _)| *tid == sel_id) {
                            let (tx, ty, _, _) = rects[idx];
                            let (col, row) =
                                point_to_cell_in_tile(scale, tx, ty, position.x, position.y, self.cell_w);
                            self.terms[idx].1.update_selection(col, row);
                        }
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
                if logical_x < SIDEBAR_WIDTH {
                    return;
                }
                #[cfg(target_os = "macos")]
                if let Some(view) = &self.input_view {
                    macos::focus_input_view(view);
                }
                let rects = tile_rects(&window, self.terms.len());
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
                    let (id, term) = &mut self.terms[idx];
                    term.clear_selection();
                    term.start_selection(col, row);
                    self.active_id = Some(id.clone());
                    self.selecting_tile = Some(id.clone());
                }
                window.request_redraw();
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.selecting_tile = None;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Positive `lines` scrolls further back into scrollback history (matches
                // `alacritty_terminal::grid::Scroll::Delta`'s convention — see
                // `TerminalSession::scroll`'s doc comment). Scrolling always targets whichever
                // tile is under the cursor, not necessarily the focused one.
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y.round() as i32,
                    MouseScrollDelta::PixelDelta(pos) => {
                        (pos.y / terminal::CELL_H as f64).round() as i32
                    }
                };
                if lines == 0 {
                    return;
                }
                let Some(window) = self.window.clone() else { return };
                let scale = window.scale_factor();
                let logical_x = self.cursor_pos.0 / scale;
                let logical_y = self.cursor_pos.1 / scale;
                let rects = tile_rects(&window, self.terms.len());
                if let Some(idx) = tile_at(&rects, logical_x, logical_y) {
                    self.terms[idx].1.scroll(lines);
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
                    let rect = Rect {
                        position: LogicalPosition::new(0.0, 0.0).into(),
                        size: LogicalSize::new(SIDEBAR_WIDTH, size.height as f64 / window.scale_factor())
                            .into(),
                    };
                    if let Some(wv) = self.webview.borrow().as_ref() {
                        let _ = wv.set_bounds(rect);
                    }
                    // Keep every tile's grid (and its pty's own `winsize`, so `SIGWINCH`-aware
                    // programs like `vim`/`htop` redraw correctly) in sync with the window —
                    // previously hardcoded to 120x40 regardless of actual window size.
                    self.refit_all_tiles(&window);
                }
            }
            // On macOS, terminal keyboard/IME input no longer flows through winit's own
            // `KeyboardInput`/`Ime` events at all — `TerminalInputView` (Phase 1d) holds
            // first responder and handles it directly, forwarding results via `AppEvent`
            // (see `user_event`). Non-macOS platforms don't have that view yet and fall
            // through to the catch-all below, which is currently a no-op (Linux keyboard
            // input for the terminal is still open — see the plan doc).
            WindowEvent::RedrawRequested => {
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
                let rects = tile_rects(window, self.terms.len());

                let mut frames = Vec::with_capacity(self.terms.len());
                for ((id, term), &(tx, ty, tw, th)) in self.terms.iter().zip(rects.iter()) {
                    let is_active = self.active_id.as_deref() == Some(id.as_str());
                    // Only the focused tile (the one that has keyboard input right now) shows
                    // a cursor and IME preedit — matches the user's own confirmed preference:
                    // each tile is an independent terminal, and the cursor marks which one
                    // you're actually typing into, same as normal window-focus behavior.
                    let preedit = if is_active { self.preedit.as_str() } else { "" };
                    let cursor_visible = is_active && self.cursor_visible;
                    let tframe = term.snapshot(preedit, cursor_visible);
                    frames.push((id.clone(), tframe, (tw, th)));

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
                            let y = ty * scale + TEXT_TOP_MARGIN * scale + row as f64 * cell_h_px;
                            if let Some(rect) =
                                macos::to_screen_rect(window, x, y, cell_w_px, cell_h_px)
                            {
                                view.set_caret_rect(rect);
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
                    .map(|(id, f, _)| {
                        (
                            id.clone(),
                            f.spans.clone(),
                            f.selection_cells.clone(),
                            f.background_cells.clone(),
                        )
                    })
                    .collect();
                if new_last == self.last_frames {
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

                    // Borders don't share glyphon's atlas (separate pipeline, its own vertex
                    // buffer per call), so drawing them per tile in a loop like this is fine —
                    // unlike the text below, there's nothing here for a later tile's draw call
                    // to invalidate out from under an earlier one.
                    for ((id, _, (tw, th)), &(tx, ty, _, _)) in frames.iter().zip(rects.iter()) {
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
                            is_active,
                            gpu.config.width,
                            gpu.config.height,
                        );
                    }

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
                        .map(|((_, tframe, _), &(tx, ty, tw, th))| {
                            let sx = (tx * scale).round() as i32;
                            let sy = (ty * scale).round() as i32;
                            let sw = (tw * scale).round() as i32;
                            let sh = (th * scale).round() as i32;
                            terminal::TileRender {
                                frame: tframe,
                                left: sx as f32 + TEXT_LEFT_MARGIN as f32 * scale as f32,
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
                }
                gpu.queue.submit(Some(encoder.finish()));
                surface_frame.present();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_blink {
            self.cursor_visible = !self.cursor_visible;
            self.next_blink = now + BLINK_INTERVAL;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        if now >= self.next_reconnect {
            if let Some(meta) = self.pending_reconnects.pop_front() {
                if let Some(window) = self.window.clone() {
                    self.spawn_session(&window, meta.id, &meta.cwd);
                    window.request_redraw();
                }
                self.next_reconnect = now + RECONNECT_STAGGER;
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

fn dev_server_url() -> &'static str {
    "http://localhost:1420"
}

fn app_data_dir() -> std::path::PathBuf {
    // Matches the directory Tauri itself used (`app.path().app_data_dir()` resolves to the
    // same OS convention keyed by the app identifier from the old tauri.conf.json), so the
    // existing termhub.sqlite from before this refactor is found in place.
    dirs::data_dir().expect("no data dir for this platform").join("com.termhub.app")
}

pub fn run() {
    std::fs::create_dir_all(app_data_dir()).expect("failed to create app data dir");
    let db = Arc::new(
        Db::open(&app_data_dir().join("termhub.sqlite")).expect("failed to open database"),
    );
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

    let activity: Activity = Arc::new(Mutex::new(HashMap::new()));
    let mut app = App::new(db, proxy, activity);
    event_loop.run_app(&mut app).expect("event loop error");
}
