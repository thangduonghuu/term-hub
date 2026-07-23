mod commands;
mod db;
mod external_terminal;
mod ipc;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_input_view;
mod session;
mod terminal;
mod usage;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
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
// Fallback grid size before the window (and its real pixel dimensions) exists —
// `compute_grid_size` replaces this with the actual fit as soon as `resumed()` runs.
const COLS: usize = 120;
const ROWS: usize = 40;
// Standard-ish terminal cursor blink rate (iTerm2/Terminal.app are both in this ballpark).
const BLINK_INTERVAL: Duration = Duration::from_millis(530);
// Must match the `left`/`top` origin used in `RedrawRequested`'s call to `gpu.text.render` —
// duplicated here (rather than computed once and stored) because it's cheap and keeps the
// margin tweakable in one place without a stale-cache field to remember to update.
const TEXT_LEFT_MARGIN: f64 = 8.0;
const TEXT_TOP_MARGIN: f64 = 4.0;

/// Maps a window-relative physical-pixel point to a display-space terminal cell
/// (column, row), using the same fixed cell-size assumption `TerminalSession::spawn` gives
/// the pty (`terminal::CELL_W`/`CELL_H`, scaled by the window's HiDPI factor) — used for
/// mouse selection and for fitting the grid to the window on resize.
fn point_to_cell(window: &Window, physical_x: f64, physical_y: f64) -> (usize, i32) {
    let scale = window.scale_factor();
    let left = SIDEBAR_WIDTH * scale + TEXT_LEFT_MARGIN * scale;
    let top = TEXT_TOP_MARGIN * scale;
    let cell_w = terminal::CELL_W as f64 * scale;
    let cell_h = terminal::CELL_H as f64 * scale;
    let col = ((physical_x - left) / cell_w).floor().max(0.0) as usize;
    let row = ((physical_y - top) / cell_h).floor().max(0.0) as i32;
    (col, row)
}

/// How many columns/rows of terminal grid fit in the window at its current size, given the
/// sidebar strip and render margins — used both for the initial terminal spawn and to keep
/// the grid (and the pty's own idea of its size, via `TerminalSession::resize`) in sync with
/// the window on every resize, which the original Phase 1 implementation never did (the grid
/// was hardcoded to 120x40 regardless of the actual window size).
fn compute_grid_size(window: &Window) -> (usize, usize) {
    let scale = window.scale_factor();
    let size = window.inner_size();
    let left = SIDEBAR_WIDTH * scale + TEXT_LEFT_MARGIN * scale;
    let top = TEXT_TOP_MARGIN * scale;
    let cell_w = terminal::CELL_W as f64 * scale;
    let cell_h = terminal::CELL_H as f64 * scale;
    let cols = (((size.width as f64) - left) / cell_w).floor().max(1.0) as usize;
    let rows = (((size.height as f64) - top) / cell_h).floor().max(1.0) as usize;
    (cols, rows)
}

/// Custom winit user event, delivered on the main thread from background threads —
/// `IpcResponse` from the IPC dispatch threads (ipc.rs), `PtyOutput` from the terminal's pty
/// reader thread (terminal.rs) so redraws are driven by actual new output instead of a
/// blind timer (which was re-shaping the whole grid ~60x/sec regardless of whether anything
/// had changed, pegging the CPU and starving the sidebar webview's own rendering).
pub enum AppEvent {
    IpcResponse(String),
    PtyOutput,
    // Sent by `macos_input_view::TerminalInputView` (Phase 1d — see the plan doc) instead of
    // winit's own `WindowEvent::Ime`/`KeyboardInput`, which had a confirmed macOS-only bug:
    // its internal IME state machine could silently drop the keystroke immediately following
    // a composition commit (e.g. the space after a Vietnamese Telex word). The custom view
    // owns keyboard input entirely on macOS now, so these carry what it decided instead.
    ImePreedit(String),
    ImeCommit(String),
    KeyControl(&'static str),
    // Sent by `TerminalInputView::doCommandBySelector` when it sees the standard Cmd+C/Cmd+V
    // key-binding selectors (`copy:`/`paste:`) — actual clipboard I/O happens here in
    // `user_event` since the view doesn't have access to `TerminalSession`.
    Copy,
    Paste,
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
    term: Option<TerminalSession>,
    // What was actually drawn last frame — `RedrawRequested` skips re-shaping/re-rendering
    // when neither has changed, since that was pegging the CPU (see the plan doc's Phase 1b
    // findings). Selection is tracked separately from the text content because dragging a
    // selection changes what should be on screen (the highlight) without changing the text
    // itself, so checking content alone would incorrectly skip those frames.
    last_content: String,
    last_selection: Vec<(usize, usize)>,
    cursor_pos: (f64, f64),
    // Live in-progress IME composition text (e.g. "ắ" while still typing Telex, before
    // it's committed) — not sent to the pty, just overlaid at the cursor when rendering so
    // it's visible as you type instead of only appearing once composition ends.
    preedit: String,
    // The custom NSTextInputClient view (Phase 1d) that owns all terminal keyboard/IME input
    // on macOS, replacing winit's own (confirmed buggy) handling entirely. Kept alive here —
    // dropping it would tear down the AppKit view it wraps.
    #[cfg(target_os = "macos")]
    input_view: Option<objc2::rc::Retained<macos_input_view::TerminalInputView>>,
    // Current grid size, kept in sync with the window via `compute_grid_size` — see that
    // function's doc comment for why this can't just stay at the `COLS`/`ROWS` constants.
    cols: usize,
    rows: usize,
    // Mouse-drag text selection (Phase 2). `mouse_selecting` is true between a `MouseInput`
    // press and release in the terminal area (not the sidebar), during which `CursorMoved`
    // extends the selection.
    mouse_selecting: bool,
    // Cursor blink phase, toggled on a timer in `about_to_wait` — see `TerminalSession::
    // snapshot`'s doc comment for why a small periodic wakeup here is fine even though this
    // app is otherwise fully event-driven.
    cursor_visible: bool,
    next_blink: Instant,
}

impl App {
    fn new(db: Arc<Db>, proxy: EventLoopProxy<AppEvent>) -> Self {
        Self {
            db,
            proxy,
            window: None,
            gpu: None,
            webview: Rc::new(RefCell::new(None)),
            term: None,
            last_content: String::new(),
            last_selection: Vec::new(),
            cursor_pos: (0.0, 0.0),
            preedit: String::new(),
            #[cfg(target_os = "macos")]
            input_view: None,
            cols: COLS,
            rows: ROWS,
            mouse_selecting: false,
            cursor_visible: true,
            next_blink: Instant::now() + BLINK_INTERVAL,
        }
    }

    /// Resets the cursor to solid/visible and pushes its next blink-off deadline out — called
    /// on every keystroke, matching the usual terminal UX where typing while the cursor
    /// happens to be in its "off" blink phase doesn't make it look unresponsive.
    fn reset_blink(&mut self) {
        self.cursor_visible = true;
        self.next_blink = Instant::now() + BLINK_INTERVAL;
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
        let text = TextPipeline::new(&device, &queue, format, config.width, config.height);

        // --- sidebar webview docked to the left strip, replacing the old Tauri-owned window ---
        let rect = Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: LogicalSize::new(SIDEBAR_WIDTH, size.height as f64 / scale).into(),
        };
        let db_for_ipc = self.db.clone();
        let proxy_for_ipc = self.proxy.clone();
        let webview = WebViewBuilder::new()
            .with_bounds(rect)
            .with_url(dev_server_url())
            .with_ipc_handler(move |msg| {
                ipc::spawn_dispatch(db_for_ipc.clone(), proxy_for_ipc.clone(), msg.body());
            })
            .build_as_child(&*window)
            .expect("failed to build sidebar webview");
        *self.webview.borrow_mut() = Some(webview);

        // --- one hardcoded terminal session (Phase 1b scope — multi-session tiling is Phase 3) ---
        let (cols, rows) = compute_grid_size(&window);
        self.cols = cols;
        self.rows = rows;
        let cwd = session::default_cwd();
        self.term = Some(
            TerminalSession::spawn(&cwd, cols, rows, self.proxy.clone())
                .expect("failed to spawn terminal"),
        );

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
            AppEvent::PtyOutput => {
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
                if let Some(term) = self.term.as_mut() {
                    term.write(&text);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            AppEvent::KeyControl(seq) => {
                self.reset_blink();
                if let Some(term) = self.term.as_mut() {
                    term.write(seq);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            AppEvent::Copy => {
                let Some(term) = self.term.as_ref() else { return };
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
                    if let Some(term) = self.term.as_mut() {
                        term.write(&text);
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x, position.y);
                if self.mouse_selecting {
                    if let Some(window) = &self.window {
                        let (col, row) = point_to_cell(window, position.x, position.y);
                        if let Some(term) = self.term.as_mut() {
                            term.update_selection(col.min(self.cols.saturating_sub(1)), row);
                        }
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // The webview child view keeps AppKit "first responder" status even once
                // the window regains key-window focus — those are separate concepts, and
                // `Window::focus_window()` only affects the latter. Explicitly hand first
                // responder back to the window's own content view on clicks outside the
                // webview's bounds, or keyboard input stays stuck routing to the webview.
                let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
                let logical_x = self.cursor_pos.0 / scale;
                if logical_x >= SIDEBAR_WIDTH {
                    #[cfg(target_os = "macos")]
                    if let Some(view) = &self.input_view {
                        macos::focus_input_view(view);
                    }
                    if let Some(window) = &self.window {
                        let (col, row) = point_to_cell(window, self.cursor_pos.0, self.cursor_pos.1);
                        if let Some(term) = self.term.as_mut() {
                            term.clear_selection();
                            term.start_selection(col.min(self.cols.saturating_sub(1)), row);
                        }
                        self.mouse_selecting = true;
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.mouse_selecting = false;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Positive `lines` scrolls further back into scrollback history (matches
                // `alacritty_terminal::grid::Scroll::Delta`'s convention — see
                // `TerminalSession::scroll`'s doc comment).
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y.round() as i32,
                    MouseScrollDelta::PixelDelta(pos) => {
                        (pos.y / terminal::CELL_H as f64).round() as i32
                    }
                };
                if lines != 0 {
                    if let Some(term) = self.term.as_mut() {
                        term.scroll(lines);
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.config.width = size.width.max(1);
                    gpu.config.height = size.height.max(1);
                    gpu.surface.configure(&gpu.device, &gpu.config);
                    gpu.text.resize(&gpu.queue, gpu.config.width, gpu.config.height);
                }
                if let Some(window) = &self.window {
                    let rect = Rect {
                        position: LogicalPosition::new(0.0, 0.0).into(),
                        size: LogicalSize::new(SIDEBAR_WIDTH, size.height as f64 / window.scale_factor())
                            .into(),
                    };
                    if let Some(wv) = self.webview.borrow().as_ref() {
                        let _ = wv.set_bounds(rect);
                    }
                    // Keep the grid (and the pty's own `winsize`, so `SIGWINCH`-aware
                    // programs like `vim`/`htop` redraw correctly) in sync with the window —
                    // previously hardcoded to 120x40 regardless of actual window size.
                    let (cols, rows) = compute_grid_size(window);
                    if (cols, rows) != (self.cols, self.rows) {
                        self.cols = cols;
                        self.rows = rows;
                        if let Some(term) = self.term.as_mut() {
                            term.resize(cols, rows);
                        }
                    }
                }
            }
            // On macOS, terminal keyboard/IME input no longer flows through winit's own
            // `KeyboardInput`/`Ime` events at all — `TerminalInputView` (Phase 1d) holds
            // first responder and handles it directly, forwarding results via `AppEvent`
            // (see `user_event`). Non-macOS platforms don't have that view yet and fall
            // through to the catch-all below, which is currently a no-op (Linux keyboard
            // input for the terminal is still open — see the plan doc).
            WindowEvent::RedrawRequested => {
                let (Some(gpu), Some(term)) = (self.gpu.as_mut(), self.term.as_ref()) else {
                    return;
                };
                let tframe = term.snapshot(&self.preedit, self.cursor_visible);
                // Re-shaping ~4800 cells' worth of text on every redraw — even when nothing
                // on screen changed — was pegging the CPU and starving the webview's own
                // rendering on the same OS run loop. Skip the whole frame when idle. Selection
                // is checked separately from content — see `last_selection`'s doc comment.
                if tframe.content == self.last_content && tframe.selection_cells == self.last_selection {
                    return;
                }
                self.last_content = tframe.content.clone();
                self.last_selection = tframe.selection_cells.clone();

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
                    // gpu.config.width/height are physical pixels (from the wgpu surface
                    // config), but SIDEBAR_WIDTH is logical (matching the webview's CSS
                    // width) — scale it or the sidebar's true physical width (e.g. 440px on
                    // a 2x display) leaves text rendered underneath the webview, hidden.
                    let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0) as f32;
                    let left = SIDEBAR_WIDTH as f32 * scale + TEXT_LEFT_MARGIN as f32 * scale;
                    gpu.text.render(
                        &gpu.device,
                        &gpu.queue,
                        &mut pass,
                        &tframe,
                        left,
                        TEXT_TOP_MARGIN as f32 * scale,
                        gpu.config.width,
                        gpu.config.height,
                        scale,
                        terminal::CELL_W * scale,
                        terminal::CELL_H * scale,
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
        // A small periodic wakeup (twice a second) to drive cursor blinking — negligible next
        // to the blind per-frame redraw timer this app deliberately moved away from (see the
        // plan doc's Phase 1b findings); everything else stays purely event-driven.
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_blink));
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

    let mut app = App::new(db, proxy);
    event_loop.run_app(&mut app).expect("event loop error");
}
