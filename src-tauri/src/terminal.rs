//! Embedded terminal: alacritty_terminal for PTY + ANSI state, glyphon for wgpu text
//! rendering. Ported from the proven `term-spike` scratch prototype (see the plan doc) —
//! same three fixes apply here: keep `Pty` alive (its `Drop` kills the shell), retry on
//! `WouldBlock` reads (the pty fd is non-blocking by design), and the caller must set
//! `ActivationPolicy::Regular` on the event loop for stable keyboard focus on macOS.
//!
//! Phase 1b scope: one hardcoded session, plain monospace text, no colors yet.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use glyphon::{
    Attrs, Buffer, Cache, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use winit::event_loop::EventLoopProxy;

use crate::AppEvent;

#[derive(Clone)]
struct NoopListener;
impl EventListener for NoopListener {
    fn send_event(&self, _event: TermEvent) {}
}

struct Dims {
    cols: usize,
    lines: usize,
}
impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub const CELL_W: f32 = 8.0;
pub const CELL_H: f32 = 16.0;

pub struct TerminalSession {
    term: Arc<Mutex<Term<NoopListener>>>,
    pty_writer: std::fs::File,
    // Must stay alive: Pty's Drop kills the child shell (see module docs).
    _pty: tty::Pty,
}

impl TerminalSession {
    pub fn spawn(
        cwd: &str,
        cols: usize,
        rows: usize,
        proxy: EventLoopProxy<AppEvent>,
    ) -> Result<Self, String> {
        let dims = Dims { cols, lines: rows };
        let term = Arc::new(Mutex::new(Term::new(TermConfig::default(), &dims, NoopListener)));

        let pty_options = tty::Options {
            shell: None,
            working_directory: Some(cwd.into()),
            drain_on_exit: true,
            env: Default::default(),
        };
        let window_size = WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: CELL_W as u16,
            cell_height: CELL_H as u16,
        };
        let pty = tty::new(&pty_options, window_size, 0).map_err(|e| e.to_string())?;
        let pty_writer = pty.file().try_clone().map_err(|e| e.to_string())?;
        let mut reader = pty.file().try_clone().map_err(|e| e.to_string())?;

        let term_for_reader = term.clone();
        std::thread::spawn(move || {
            let mut processor = Processor::<StdSyncHandler>::new();
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut term = term_for_reader.lock().unwrap();
                        processor.advance(&mut *term, &buf[..n]);
                        drop(term);
                        // Only wake the render loop when there's actually new output to
                        // show, instead of redrawing (and re-shaping the whole grid) on a
                        // blind timer regardless of whether anything changed.
                        let _ = proxy.send_event(AppEvent::PtyOutput);
                    }
                    // The pty fd is non-blocking (alacritty's own code drives it via its own
                    // polling event loop, which we bypass in favor of this simple thread) —
                    // no data ready yet isn't an error, just retry.
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self { term, pty_writer, _pty: pty })
    }

    pub fn write(&mut self, s: &str) {
        let _ = self.pty_writer.write_all(s.as_bytes());
    }

    /// Plain-text snapshot of the current grid, rows joined by '\n'. Monochrome by design —
    /// per-cell ANSI colors were tried and explicitly rejected in favor of a flat black &
    /// white look (matching iTerm2's default monochrome profile).
    ///
    /// `preedit` (macOS IME in-progress composition text, e.g. "ắ" while still typing
    /// Telex, before it's committed) is overlaid at the cursor position so it's visible
    /// live as you type, matching how a native app would show it — it isn't part of the
    /// real terminal buffer yet (nothing has been sent to the pty for it), just a visual
    /// preview spliced in at render time. A block cursor is overlaid right after it (or at
    /// the raw cursor position when there's no active composition) — there's no separate
    /// cursor-drawing pass, it's just another character substitution like preedit.
    pub fn snapshot_text_with_preedit(&self, preedit: &str) -> String {
        let term = self.term.lock().unwrap();
        let grid = term.grid();
        let cols = grid.columns();
        let cursor_line = grid.cursor.point.line.0;
        let cursor_col = grid.cursor.point.column.0;
        let preedit_chars: Vec<char> = preedit.chars().collect();
        let cursor_display_col = cursor_col + preedit_chars.len();
        let mut s = String::new();
        for (row_idx, row) in grid.display_iter().collect::<Vec<_>>().chunks(cols).enumerate() {
            for (col_idx, cell) in row.iter().enumerate() {
                let in_preedit = !preedit_chars.is_empty()
                    && row_idx as i32 == cursor_line
                    && col_idx >= cursor_col
                    && col_idx < cursor_col + preedit_chars.len();
                let is_cursor = row_idx as i32 == cursor_line && col_idx == cursor_display_col;
                if in_preedit {
                    s.push(preedit_chars[col_idx - cursor_col]);
                } else if is_cursor {
                    s.push('█');
                } else {
                    s.push(cell.c);
                }
            }
            s.push('\n');
        }
        s
    }
}

pub struct TextPipeline {
    font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    viewport: Viewport,
}

impl TextPipeline {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
        let font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(device);
        let mut viewport = Viewport::new(device, &cache);
        viewport.update(queue, Resolution { width, height });
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        Self { font_system, swash_cache, atlas, text_renderer, viewport }
    }

    pub fn resize(&mut self, queue: &wgpu::Queue, width: u32, height: u32) {
        self.viewport.update(queue, Resolution { width, height });
    }

    /// Renders `content` (left-aligned, top-left origin) into `view` within `pass`, in a
    /// flat monochrome color (no per-cell ANSI colors — see `snapshot_text`'s doc comment).
    ///
    /// `width`/`height`/`left`/`top` are physical pixels (matching the wgpu surface), so
    /// `scale_factor` (the window's HiDPI scale) must be applied to the font metrics too —
    /// otherwise a 14pt font renders at roughly half its intended visual size on a 2x
    /// display, since it'd be laid out as if 14 *physical* px in a canvas that's actually
    /// twice as many physical pixels per logical point.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass,
        content: &str,
        left: f32,
        top: f32,
        width: u32,
        height: u32,
        scale_factor: f32,
    ) {
        let metrics = Metrics::new(14.0 * scale_factor, CELL_H * scale_factor);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(&mut self.font_system, Some(width as f32), Some(height as f32));
        buffer.set_text(
            &mut self.font_system,
            content,
            // The generic Monospace family resolves to whatever the system default is
            // (Menlo/SF Mono on macOS), which is missing about half of the Vietnamese
            // precomposed Latin block (confirmed earlier this session via direct fontconfig
            // inspection) — e.g. "ắ" (U+1EAF) renders as a tofu box. Monaco, also bundled
            // with macOS, has full coverage (confirmed 0/90 missing at the time).
            &Attrs::new().family(Family::Name("Monaco")),
            // Basic is far cheaper than Advanced (skips full BiDi/complex-script analysis)
            // and is enough for a terminal grid — precomposed Vietnamese/Latin diacritics
            // etc. still render correctly, they just don't need script reordering.
            Shaping::Basic,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        let text_area = TextArea {
            buffer: &buffer,
            left,
            top,
            scale: 1.0,
            bounds: TextBounds { left: 0, top: 0, right: width as i32, bottom: height as i32 },
            // iTerm2's default profile: light gray foreground (not pure white — easier on
            // the eyes against pure black than full-contrast white).
            default_color: glyphon::Color::rgb(208, 208, 208),
            custom_glyphs: &[],
        };

        self.text_renderer
            .prepare(
                device,
                queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                [text_area],
                &mut self.swash_cache,
            )
            .unwrap();
        self.text_renderer.render(&self.atlas, &self.viewport, pass).unwrap();
        self.atlas.trim();
    }
}
