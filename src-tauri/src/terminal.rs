//! Embedded terminal: alacritty_terminal for PTY + ANSI state, glyphon for wgpu text
//! rendering. Ported from the proven `term-spike` scratch prototype (see the plan doc) —
//! same three fixes apply here: keep `Pty` alive (its `Drop` kills the shell), retry on
//! `WouldBlock` reads (the pty fd is non-blocking by design), and the caller must set
//! `ActivationPolicy::Regular` on the event loop for stable keyboard focus on macOS.
//!
//! Phase 2 scope: resize, cursor shape/blink, scrollback + scroll input, mouse selection +
//! clipboard copy/paste. Still monochrome by design (see `render`'s doc comment).

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as TermEvent, EventListener, OnResize, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle, Processor, StdSyncHandler};
use glyphon::{
    Attrs, Buffer, Cache, Color as TextColor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use wgpu::util::DeviceExt;
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

/// A snapshot of what to draw for one frame: the plain-text grid (with preedit and the
/// cursor's glyph substitution already spliced in, same trick as before — simplest way to
/// draw a cursor without a second render pass), plus the display-space cell coordinates
/// currently under the selection, drawn as highlight rectangles by `SelectionPipeline`
/// *underneath* the text in the same render pass.
pub struct Frame {
    pub content: String,
    pub selection_cells: Vec<(usize, usize)>,
}

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
        // `alacritty_terminal`'s own default is a solid (non-blinking) cursor — it only
        // blinks if the running program explicitly requests it via `CSI ? 12 h`/DECSCUSR,
        // which most shells never do. Real terminal apps make blinking the *app-level*
        // default instead (a user preference, not something every program must opt into), and a
        // program can still override it — `cursor_style()` only falls back to this default
        // when the program hasn't set its own style.
        let term_config = TermConfig {
            default_cursor_style: CursorStyle { shape: CursorShape::Block, blinking: true },
            ..TermConfig::default()
        };
        let term = Arc::new(Mutex::new(Term::new(term_config, &dims, NoopListener)));

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

    /// Resizes both the terminal grid and the pty's own notion of its window size (the latter
    /// via `SIGWINCH`, delivered by the kernel once `on_resize` sets the pty's `winsize` —
    /// without it, programs like `vim`/`htop` that query the terminal size on startup or via
    /// `SIGWINCH` keep drawing for the old dimensions after a window resize).
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let dims = Dims { cols, lines: rows };
        self.term.lock().unwrap().resize(dims);
        let window_size = WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: CELL_W as u16,
            cell_height: CELL_H as u16,
        };
        self._pty.on_resize(window_size);
    }

    /// Scrolls the viewport by `delta` lines (positive = toward history/up, negative = toward
    /// the live prompt/down) — a thin wrapper since `alacritty_terminal`'s `Grid` already
    /// tracks scrollback and `display_iter()` (used by `snapshot`) automatically renders from
    /// the scrolled position, no separate scrollback buffer or rendering path needed.
    pub fn scroll(&mut self, delta: i32) {
        self.term.lock().unwrap().scroll_display(Scroll::Delta(delta));
    }

    /// Starts a new selection at the given display-space cell (`row` is 0 at the top of what's
    /// currently visible, negative/positive scroll doesn't change that — the conversion to
    /// `alacritty_terminal`'s absolute grid `Line` happens here via `display_offset`).
    pub fn start_selection(&mut self, col: usize, row: i32) {
        let mut term = self.term.lock().unwrap();
        let offset = term.grid().display_offset() as i32;
        let point = Point::new(Line(row - offset), Column(col));
        term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
    }

    /// Extends the in-progress selection (started via `start_selection`) to the given
    /// display-space cell — called on every `CursorMoved` while the mouse button is held.
    pub fn update_selection(&mut self, col: usize, row: i32) {
        let mut term = self.term.lock().unwrap();
        let offset = term.grid().display_offset() as i32;
        let point = Point::new(Line(row - offset), Column(col));
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, Side::Left);
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.lock().unwrap().selection = None;
    }

    pub fn selection_to_string(&self) -> Option<String> {
        self.term.lock().unwrap().selection_to_string()
    }

    /// Builds the frame to render: plain-text grid with `preedit` (in-progress macOS IME
    /// composition text, e.g. "ắ" while still typing Telex, before it's committed — see
    /// `TerminalInputView` in `macos_input_view.rs`) and the cursor overlaid at the terminal's
    /// actual cursor position, plus which display-space cells are currently selected.
    ///
    /// The cursor's on-screen glyph depends on `CursorShape` (block/underline/beam, as set by
    /// the running program via `CSI q`) and blink phase (`cursor_visible`, driven by a timer in
    /// `lib.rs` — real blinking needs periodic redraws even though this app is otherwise fully
    /// event-driven, but at ~2 wakeups/sec that's negligible next to the CPU cost the blind
    /// per-frame redraw timer had before it was replaced with event-driven redraws). The
    /// cursor and preedit are only shown at `display_offset() == 0` (not scrolled into
    /// history) — real terminals hide the cursor while scrolled back too, since it isn't
    /// actually visible there.
    pub fn snapshot(&self, preedit: &str, cursor_visible: bool) -> Frame {
        let term = self.term.lock().unwrap();
        let grid = term.grid();
        let cols = grid.columns();
        let scrolled_back = grid.display_offset() != 0;
        let cursor_line = grid.cursor.point.line.0;
        let cursor_col = grid.cursor.point.column.0;
        let preedit_chars: Vec<char> = preedit.chars().collect();
        let cursor_display_col = cursor_col + preedit_chars.len();
        let cursor_style = term.cursor_style();
        let cursor_glyph = match cursor_style.shape {
            CursorShape::Block | CursorShape::HollowBlock => '█',
            CursorShape::Underline => '_',
            CursorShape::Beam => '│',
            CursorShape::Hidden => ' ',
        };
        let show_cursor =
            !scrolled_back && (cursor_visible || !cursor_style.blinking) && cursor_style.shape != CursorShape::Hidden;

        let selection_range = term.selection.as_ref().and_then(|s| s.to_range(&term));

        let mut content = String::new();
        let mut selection_cells = Vec::new();
        for (row_idx, row) in grid.display_iter().collect::<Vec<_>>().chunks(cols).enumerate() {
            for (col_idx, cell) in row.iter().enumerate() {
                let in_preedit = !scrolled_back
                    && !preedit_chars.is_empty()
                    && row_idx as i32 == cursor_line
                    && col_idx >= cursor_col
                    && col_idx < cursor_col + preedit_chars.len();
                let is_cursor =
                    show_cursor && row_idx as i32 == cursor_line && col_idx == cursor_display_col;
                if in_preedit {
                    content.push(preedit_chars[col_idx - cursor_col]);
                } else if is_cursor {
                    content.push(cursor_glyph);
                } else {
                    content.push(cell.c);
                }

                if let Some(range) = &selection_range {
                    if range.contains(cell.point) {
                        selection_cells.push((row_idx, col_idx));
                    }
                }
            }
            content.push('\n');
        }
        Frame { content, selection_cells }
    }
}

pub struct TextPipeline {
    font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    viewport: Viewport,
    selection: SelectionPipeline,
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
        let selection = SelectionPipeline::new(device, format);
        Self { font_system, swash_cache, atlas, text_renderer, viewport, selection }
    }

    pub fn resize(&mut self, queue: &wgpu::Queue, width: u32, height: u32) {
        self.viewport.update(queue, Resolution { width, height });
    }

    /// Renders `frame` (left-aligned, top-left origin) into `view` within `pass`, in a flat
    /// monochrome color (no per-cell ANSI colors — see the plan doc for why: tried and
    /// explicitly rejected in favor of a flat black & white look matching iTerm2's default
    /// monochrome profile). Selection highlight rectangles are drawn first so glyphs render on
    /// top of them, matching how every other terminal composites selection.
    ///
    /// `width`/`height`/`left`/`top` are physical pixels (matching the wgpu surface), so
    /// `scale_factor` (the window's HiDPI scale) must be applied to the font metrics too —
    /// otherwise a 14pt font renders at roughly half its intended visual size on a 2x
    /// display, since it'd be laid out as if 14 *physical* px in a canvas that's actually
    /// twice as many physical pixels per logical point.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass,
        frame: &Frame,
        left: f32,
        top: f32,
        width: u32,
        height: u32,
        scale_factor: f32,
        cell_w: f32,
        cell_h: f32,
    ) {
        if !frame.selection_cells.is_empty() {
            self.selection.render(
                device,
                queue,
                pass,
                &frame.selection_cells,
                left,
                top,
                cell_w,
                cell_h,
                width,
                height,
            );
        }

        let metrics = Metrics::new(14.0 * scale_factor, CELL_H * scale_factor);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(&mut self.font_system, Some(width as f32), Some(height as f32));
        buffer.set_text(
            &mut self.font_system,
            &frame.content,
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
            default_color: TextColor::rgb(208, 208, 208),
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

/// A minimal solid-quad wgpu pipeline for the selection highlight — glyphon only draws text,
/// so per-cell background rectangles (real terminals render selection as a highlighted
/// background, not just differently colored text) need this tiny separate pipeline. Vertices
/// are built directly in clip space (NDC) on the CPU each frame — simplest possible approach
/// given selection changes are rare (mouse-drag driven) and the cell count is small (a few
/// hundred at most), no need for instancing or a persistent vertex buffer.
struct SelectionPipeline {
    pipeline: wgpu::RenderPipeline,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SelectionVertex {
    position: [f32; 2],
    color: [f32; 4],
}

const SELECTION_SHADER: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};
@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = vec4<f32>(in.position, 0.0, 1.0);
    out.color = in.color;
    return out;
}
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

impl SelectionPipeline {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("selection-shader"),
            source: wgpu::ShaderSource::Wgsl(SELECTION_SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("selection-pipeline-layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("selection-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<SelectionVertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: std::mem::size_of::<[f32; 2]>() as wgpu::BufferAddress,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        Self { pipeline }
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        &self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass,
        cells: &[(usize, usize)],
        left: f32,
        top: f32,
        cell_w: f32,
        cell_h: f32,
        viewport_w: u32,
        viewport_h: u32,
    ) {
        let to_ndc = |x: f32, y: f32| -> [f32; 2] {
            [
                (x / viewport_w as f32) * 2.0 - 1.0,
                1.0 - (y / viewport_h as f32) * 2.0,
            ]
        };
        // Selection tint: light, semi-transparent gray — lets the glyph drawn on top (in
        // this app's monochrome light-gray-on-black palette) stay legible.
        let color = [1.0, 1.0, 1.0, 0.28];
        let mut vertices = Vec::with_capacity(cells.len() * 6);
        for &(row, col) in cells {
            let x0 = left + col as f32 * cell_w;
            let y0 = top + row as f32 * cell_h;
            let x1 = x0 + cell_w;
            let y1 = y0 + cell_h;
            let tl = SelectionVertex { position: to_ndc(x0, y0), color };
            let tr = SelectionVertex { position: to_ndc(x1, y0), color };
            let bl = SelectionVertex { position: to_ndc(x0, y1), color };
            let br = SelectionVertex { position: to_ndc(x1, y1), color };
            vertices.extend_from_slice(&[tl, bl, tr, tr, bl, br]);
        }

        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("selection-vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, buffer.slice(..));
        pass.draw(0..vertices.len() as u32, 0..1);
    }
}
