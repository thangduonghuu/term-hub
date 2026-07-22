mod commands;
mod db;
mod external_terminal;
mod ipc;
#[cfg(target_os = "macos")]
mod macos;
mod session;
mod terminal;
mod usage;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use db::Db;
use terminal::{TerminalSession, TextPipeline};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

const SIDEBAR_WIDTH: f64 = 220.0;
const COLS: usize = 120;
const ROWS: usize = 40;

/// Custom winit user event, delivered on the main thread from background threads —
/// `IpcResponse` from the IPC dispatch threads (ipc.rs), `PtyOutput` from the terminal's pty
/// reader thread (terminal.rs) so redraws are driven by actual new output instead of a
/// blind timer (which was re-shaping the whole grid ~60x/sec regardless of whether anything
/// had changed, pegging the CPU and starving the sidebar webview's own rendering).
pub enum AppEvent {
    IpcResponse(String),
    PtyOutput,
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
    last_logged: String,
    cursor_pos: (f64, f64),
    // Erase-on-commit safety net: if a plain (non-IME) character was just written and an
    // IME composition then commits without ever going through `Ime::Preedit` (some input
    // methods retroactively replace already-committed text via
    // `insertText:replacementRange:` instead of live-previewing it — winit's macOS backend
    // ignores that replacementRange, a confirmed upstream bug, rust-windowing/winit#3617),
    // this lets us erase the stale plain character ourselves before writing the real
    // composed text. Cleared whenever a `Preedit` session starts, since macOS Vietnamese
    // Telex (confirmed via testing) normally composes live through `Preedit` — nothing is
    // written to the pty for the characters it owns, so there's nothing to erase.
    last_ime_seed: String,
    // Live in-progress IME composition text (e.g. "ắ" while still typing Telex, before
    // it's committed) — not sent to the pty, just overlaid at the cursor when rendering so
    // it's visible as you type instead of only appearing once composition ends.
    preedit: String,
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
            last_logged: String::new(),
            cursor_pos: (0.0, 0.0),
            last_ime_seed: String::new(),
            preedit: String::new(),
        }
    }
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("TermHub")
            .with_inner_size(winit::dpi::LogicalSize::new(1100.0, 700.0));
        let window = Arc::new(event_loop.create_window(attrs).unwrap());
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
        let cwd = session::default_cwd();
        self.term = Some(
            TerminalSession::spawn(&cwd, COLS, ROWS, self.proxy.clone())
                .expect("failed to spawn terminal"),
        );

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
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x, position.y);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, .. } => {
                // The webview child view keeps AppKit "first responder" status even once
                // the window regains key-window focus — those are separate concepts, and
                // `Window::focus_window()` only affects the latter. Explicitly hand first
                // responder back to the window's own content view on clicks outside the
                // webview's bounds, or keyboard input stays stuck routing to the webview.
                let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
                let logical_x = self.cursor_pos.0 / scale;
                if logical_x >= SIDEBAR_WIDTH {
                    if let Some(w) = &self.window {
                        #[cfg(target_os = "macos")]
                        macos::reclaim_first_responder(w);
                        // Re-assert IME support after the manual first-responder change
                        // above — winit's own IME/input-context activation may be tied to
                        // its own internal focus-change flow, which the raw AppKit call
                        // bypasses, otherwise Vietnamese/IME composition stops working even
                        // though plain ASCII keys still come through fine.
                        w.set_ime_allowed(true);
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
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                let Some(term) = self.term.as_mut() else { return };
                // Named keys with a specific control sequence the shell expects (e.g.
                // Backspace as DEL/0x7f, not whatever raw char winit's `text` reports —
                // that was 0x08/BS, which most shells don't treat as erase) take priority
                // over `event.text`. Everything else falls through to `event.text`, which
                // is the IME-composed string for the keystroke — the piece that makes
                // Vietnamese/any-language input work, since it comes from the OS's real
                // text-input handling.
                let named_seq: Option<&str> = match event.logical_key {
                    Key::Named(NamedKey::Enter) => Some("\r"),
                    Key::Named(NamedKey::Backspace) => Some("\x7f"),
                    Key::Named(NamedKey::Tab) => Some("\t"),
                    Key::Named(NamedKey::ArrowLeft) => Some("\x1b[D"),
                    Key::Named(NamedKey::ArrowRight) => Some("\x1b[C"),
                    Key::Named(NamedKey::ArrowUp) => Some("\x1b[A"),
                    Key::Named(NamedKey::ArrowDown) => Some("\x1b[B"),
                    _ => None,
                };
                if let Some(seq) = named_seq {
                    term.write(seq);
                    self.last_ime_seed.clear();
                } else if let Some(text) = &event.text {
                    term.write(text.as_str());
                    self.last_ime_seed = text.to_string();
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Ime(Ime::Preedit(text, _cursor)) => {
                self.preedit = text;
                if !self.preedit.is_empty() {
                    // A live composition session is now driving input; whatever plain
                    // character was last written is no longer relevant to erase.
                    self.last_ime_seed.clear();
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.preedit.clear();
                let Some(term) = self.term.as_mut() else { return };
                // Safety net for IMEs that skip Preedit and retroactively replace an
                // already-committed plain character instead (see `last_ime_seed`'s doc
                // comment) — a no-op backspace count when, as is normal for Vietnamese
                // Telex, composition went through Preedit and nothing was written yet.
                let backspaces = "\x7f".repeat(self.last_ime_seed.chars().count());
                term.write(&backspaces);
                term.write(&text);
                self.last_ime_seed.clear();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Ime(_) => {}
            WindowEvent::RedrawRequested => {
                let (Some(gpu), Some(term)) = (self.gpu.as_mut(), self.term.as_ref()) else {
                    return;
                };
                let content = term.snapshot_text_with_preedit(&self.preedit);
                // Re-shaping ~4800 cells' worth of text on every redraw — even when nothing
                // on screen changed — was pegging the CPU and starving the webview's own
                // rendering on the same OS run loop. Skip the whole frame when idle.
                if content == self.last_logged {
                    return;
                }
                self.last_logged = content.clone();

                let frame = gpu.surface.get_current_texture().unwrap();
                let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
                let mut encoder =
                    gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color {
                                    r: 0.078,
                                    g: 0.078,
                                    b: 0.078,
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
                    let left = SIDEBAR_WIDTH as f32 * scale + 8.0 * scale;
                    gpu.text.render(
                        &gpu.device,
                        &gpu.queue,
                        &mut pass,
                        &content,
                        left,
                        4.0 * scale,
                        gpu.config.width,
                        gpu.config.height,
                        scale,
                    );
                }
                gpu.queue.submit(Some(encoder.finish()));
                frame.present();
            }
            _ => {}
        }
    }

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
