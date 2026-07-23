//! Embedded terminal: alacritty_terminal for PTY + ANSI state, glyphon for wgpu text
//! rendering. Ported from the proven `term-spike` scratch prototype (see the plan doc) —
//! same three fixes apply here: keep `Pty` alive (its `Drop` kills the shell), retry on
//! `WouldBlock` reads (the pty fd is non-blocking by design), and the caller must set
//! `ActivationPolicy::Regular` on the event loop for stable keyboard focus on macOS.
//!
//! Phase 2 scope: resize, cursor shape/blink, scrollback + scroll input, mouse selection +
//! clipboard copy/paste. ANSI foreground colors are rendered (see `resolve_fg`); background
//! colors and reverse-video aren't (would need a per-cell background quad pass, like
//! selection highlighting's, which isn't implemented — out of scope so far).

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as TermEvent, EventListener, OnResize, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorShape, CursorStyle, Processor, StdSyncHandler,
};
use glyphon::{
    Attrs, Buffer, Cache, Color as TextColor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use wgpu::util::DeviceExt;
use winit::event_loop::EventLoopProxy;

use crate::AppEvent;

/// Answers escape-sequence *queries* the terminal is expected to reply to over the pty —
/// device attributes, cursor position reports, OSC color queries, etc. Discovered as a real,
/// serious bug: an earlier version of this listener was a total no-op, silently dropping
/// every `Event::PtyWrite`/`Event::ColorRequest` `alacritty_terminal` produces. That's
/// invisible for a plain shell prompt (nothing asks), but plenty of real programs (an AI CLI's
/// interactive TUI was what actually surfaced this) send a query like `CSI 6n` (cursor
/// position report) as part of their own terminal-capability probing at startup and then
/// *block waiting for the reply* — with nothing ever answering, they hang forever, which
/// looked indistinguishable from "the program won't start" from the outside.
struct PtyEventListener {
    pty_writer: std::fs::File,
}

impl EventListener for PtyEventListener {
    fn send_event(&self, event: TermEvent) {
        match event {
            TermEvent::PtyWrite(text) => {
                let _ = (&self.pty_writer).write_all(text.as_bytes());
            }
            // OSC color queries (`\x1b]4;...`, `\x1b]10;...`, etc.) — same "must answer or the
            // asker may hang" concern as PtyWrite above. We don't track a real customizable
            // palette, so answer with whatever `resolve_fg`/`indexed_to_rgb` would render that
            // index as, which is at least self-consistent with what's actually on screen.
            TermEvent::ColorRequest(index, format) => {
                let rgb = match index {
                    // Matches `alacritty_terminal::vte::ansi::NamedColor`'s discriminants for
                    // the special (non-indexed) color slots.
                    256 => (208, 208, 208),                     // Foreground (this app's default)
                    257 => (0, 0, 0),                            // Background
                    258 => (208, 208, 208),                      // Cursor
                    n if n < 256 => indexed_to_rgb(n as u8),
                    _ => (208, 208, 208),
                };
                let reply = format(alacritty_terminal::vte::ansi::Rgb {
                    r: rgb.0,
                    g: rgb.1,
                    b: rgb.2,
                });
                let _ = (&self.pty_writer).write_all(reply.as_bytes());
            }
            _ => {}
        }
    }
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

// Fallback only — `TextPipeline::measure_cell_width` gives the real value, measured from the
// actual font, and callers should prefer that. This stays around as what `measure_cell_width`
// itself falls back to if shaping somehow produces no glyph run at all.
pub const CELL_W: f32 = 8.0;
pub const CELL_H: f32 = 16.0;

/// A snapshot of what to draw for one frame: the grid's text (with preedit and the cursor's
/// glyph substitution already spliced in, same trick as before — simplest way to draw a
/// cursor without a second render pass) broken into `spans` — consecutive runs of text
/// sharing one foreground color, `None` meaning "the default" — plus the display-space cell
/// coordinates currently under the selection, drawn as highlight rectangles by
/// `SelectionPipeline` *underneath* the text in the same render pass.
pub struct Frame {
    pub spans: Vec<(String, Option<(u8, u8, u8)>)>,
    pub selection_cells: Vec<(usize, usize)>,
    /// Cells with a non-default background color, drawn as solid (fully opaque) rectangles
    /// *underneath* the text and selection highlight — needed for anything that leans on
    /// per-cell background fills rather than just colored text: `less`/`man`'s reverse-video
    /// header bars, reverse-video generally (`Flags::INVERSE`, folded in here at snapshot
    /// time rather than needing a separate code path), and terminal image-rendering tricks
    /// (half-block Unicode glyphs with fg/bg set per cell to double vertical resolution —
    /// confirmed via a real example: a `neofetch` ASCII-art banner rendered as blank/missing
    /// in this app before background support existed, since it's built almost entirely out of
    /// per-cell background color, not colored glyphs).
    pub background_cells: Vec<(usize, usize, (u8, u8, u8))>,
}

/// The standard 16-color ANSI/xterm palette (0-7 normal, 8-15 bright) — matches VS Code's
/// default terminal theme, a reasonable, readable choice consistent with this app's existing
/// "clean, iTerm2-ish" aesthetic rather than the harsher fully-saturated classic xterm colors.
const ANSI_16: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 49, 49),
    (13, 188, 121),
    (229, 229, 16),
    (36, 114, 200),
    (188, 63, 188),
    (17, 168, 205),
    (229, 229, 229),
    (102, 102, 102),
    (241, 76, 76),
    (35, 209, 139),
    (245, 245, 67),
    (59, 142, 234),
    (214, 112, 214),
    (41, 184, 219),
    (229, 229, 229),
];

/// Expands an xterm 256-color palette index (0-15 the 16 ANSI colors, 16-231 a 6x6x6 color
/// cube, 232-255 a grayscale ramp) to RGB — the standard xterm formula, not specific to this
/// app.
fn indexed_to_rgb(n: u8) -> (u8, u8, u8) {
    if n < 16 {
        ANSI_16[n as usize]
    } else if n < 232 {
        let n = n - 16;
        let scale = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
        (scale(n / 36), scale((n / 6) % 6), scale(n % 6))
    } else {
        let v = 8 + (n - 232) * 10;
        (v, v, v)
    }
}

/// Resolves a cell's foreground color to concrete RGB, or `None` to mean "use the terminal's
/// default text color" (`Color::Named(NamedColor::Foreground)`, what an untouched cell has,
/// and also anything this function doesn't specifically handle, e.g. `Cursor`/`*Foreground`
/// variants that don't make sense as a literal color here). `Flags::BOLD` brightens one of the
/// 8 base ANSI colors to its bright counterpart, the near-universal terminal convention (most
/// themes/tools, e.g. `ls`/git, rely on exactly this to get 16 visually distinct colors out of
/// 8 color codes).
fn resolve_fg(fg: AnsiColor, flags: Flags) -> Option<(u8, u8, u8)> {
    let bold = flags.contains(Flags::BOLD);
    match fg {
        AnsiColor::Named(named) => {
            let idx = named as usize;
            if idx < 16 {
                Some(ANSI_16[if bold && idx < 8 { idx + 8 } else { idx }])
            } else {
                None
            }
        }
        AnsiColor::Indexed(n) => Some(indexed_to_rgb(if bold && n < 8 { n + 8 } else { n })),
        AnsiColor::Spec(rgb) => Some((rgb.r, rgb.g, rgb.b)),
    }
}

/// Same idea as `resolve_fg`, but for `cell.bg` — `None` means "the default background"
/// (this app's flat black, already what the whole frame is cleared to, so nothing extra
/// needs to be drawn for it). No bold-brightening here — that convention is specifically
/// about foreground text legibility, not backgrounds.
fn resolve_bg(bg: AnsiColor) -> Option<(u8, u8, u8)> {
    match bg {
        AnsiColor::Named(named) => {
            let idx = named as usize;
            if idx < 16 {
                Some(ANSI_16[idx])
            } else {
                None
            }
        }
        AnsiColor::Indexed(n) => Some(indexed_to_rgb(n)),
        AnsiColor::Spec(rgb) => Some((rgb.r, rgb.g, rgb.b)),
    }
}

pub struct TerminalSession {
    term: Arc<Mutex<Term<PtyEventListener>>>,
    pty_writer: std::fs::File,
    // Must stay alive: Pty's Drop kills the child shell (see module docs).
    _pty: tty::Pty,
}

impl TerminalSession {
    pub fn spawn(
        id: String,
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

        // `alacritty_terminal` doesn't set any of these itself, and this app is normally
        // launched (during development) via `open` from an already-running terminal, so the
        // spawned shell would otherwise silently inherit *that* terminal's identity. `TERM`/
        // `COLORTERM` are set explicitly rather than hoping they're already correct in the
        // inherited environment — `xterm-256color` is the safe, near-universally-recognized
        // choice, and `truecolor` is accurate for this app (`resolve_fg` does render 24-bit
        // `Color::Spec` values exactly, not just palette-approximated).
        //
        // `TERM_PROGRAM` is deliberately claimed as `iTerm.app`, not some unique "TermHub"
        // identity — confirmed via `~/.kiro/settings/cli.json`, kiro-cli's shell integration
        // only activates for a small allowlist of recognized terminals (`integrations.iterm`,
        // `integrations.terminal`, `integrations.vscode`), keyed off exactly this variable; an
        // unrecognized value means it silently no-ops instead of wrapping the shell for its
        // inline-suggestion feature. This is the well-precedented way less-common terminals
        // get compatibility with tools that gate features on `$TERM_PROGRAM` rather than
        // actual capability detection — not unique to this app or this integration.
        let mut env = std::collections::HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        env.insert("TERM_PROGRAM".to_string(), "iTerm.app".to_string());
        // `tty::Options.env` only *adds/overrides* vars, it can't remove an inherited one —
        // but setting these to an empty string has the same effect for every shell-script
        // `-z "$VAR"` check that matters here (though *not* necessarily for a compiled
        // binary's own env lookup — confirmed the hard way: including `PROCESS_LAUNCHED_BY_Q`
        // in this list at first broke a *different* check than the one below, since some
        // programs distinguish "set to empty" from "not set at all" even where a shell script
        // wouldn't; leave anything you're not sure a consumer treats identically both ways
        // off this list rather than clearing it defensively). Confirmed real bug (same root
        // cause as `TERM_PROGRAM` above, different symptom): launching TermHub from an
        // already-running terminal whose own shell integration had set `Q_TERM`/session-
        // tracking vars for *itself* leaked those values down into every session TermHub
        // spawns. One shell-integration tool's re-wrap-prevention check (`-z "$Q_TERM"`,
        // meant to detect "am I already wrapped, so don't wrap again") then saw a stale,
        // inherited non-empty value and concluded a from-scratch TermHub session was "already
        // wrapped", silently skipping initialization of a feature (inline suggestions) that
        // never actually ran here.
        // `NEOFETCH_SHOWN` isn't a kiro-cli var at all — it's this user's own `.zshrc`
        // convention for "only show the startup banner once" — but it's exactly the same
        // leakage pattern as the vars above: without clearing it, every TermHub session
        // inherits "already shown" from whatever shell launched TermHub itself and never
        // shows its own banner, even on what should look like a brand new terminal.
        for stale in
            ["Q_TERM", "Q_TERM_TMUX", "QTERM_SESSION_ID", "Q_PARENT", "NEOFETCH_SHOWN"]
        {
            env.insert(stale.to_string(), String::new());
        }
        let pty_options = tty::Options {
            shell: None,
            working_directory: Some(cwd.into()),
            drain_on_exit: true,
            env,
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

        // The listener needs its own pty write handle (separate from `pty_writer` below, which
        // is for the app's own outgoing keystrokes) so it can answer terminal-capability
        // queries the running program sends — see `PtyEventListener`'s doc comment for why
        // this matters at all.
        let listener_writer = pty.file().try_clone().map_err(|e| e.to_string())?;
        let term = Arc::new(Mutex::new(Term::new(
            term_config,
            &dims,
            PtyEventListener { pty_writer: listener_writer },
        )));

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
                        // blind timer regardless of whether anything changed. Carries `id` so
                        // `lib.rs` can also record which session's activity dot to light up in
                        // the sidebar (Phase 4).
                        let _ = proxy.send_event(AppEvent::PtyOutput(id.clone()));
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

    /// The cursor's current display-space (col, row) — `None` when scrolled back into
    /// history, matching `snapshot`'s own rule for when the cursor isn't actually shown.
    /// Exists purely so `lib.rs` can tell the OS where the terminal's text caret is on
    /// screen, for `NSAccessibility` queries (see `macos_input_view`'s doc comment) — nothing
    /// here affects what's actually rendered.
    pub fn cursor_position(&self) -> Option<(usize, i32)> {
        let term = self.term.lock().unwrap();
        let grid = term.grid();
        if grid.display_offset() != 0 {
            return None;
        }
        Some((grid.cursor.point.column.0, grid.cursor.point.line.0))
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

        let mut spans: Vec<(String, Option<(u8, u8, u8)>)> = Vec::new();
        let mut cur_color: Option<(u8, u8, u8)> = None;
        let mut cur_text = String::new();
        let mut selection_cells = Vec::new();
        let mut background_cells = Vec::new();
        for (row_idx, row) in grid.display_iter().collect::<Vec<_>>().chunks(cols).enumerate() {
            for (col_idx, cell) in row.iter().enumerate() {
                let in_preedit = !scrolled_back
                    && !preedit_chars.is_empty()
                    && row_idx as i32 == cursor_line
                    && col_idx >= cursor_col
                    && col_idx < cursor_col + preedit_chars.len();
                let is_cursor =
                    show_cursor && row_idx as i32 == cursor_line && col_idx == cursor_display_col;
                // Preedit/cursor glyphs are UI overlays, not real terminal content — always
                // drawn in the default color (and no background fill) rather than inheriting
                // whatever the underlying cell's color happened to be.
                let (ch, color) = if in_preedit {
                    (preedit_chars[col_idx - cursor_col], None)
                } else if is_cursor {
                    (cursor_glyph, None)
                } else {
                    let mut fg = resolve_fg(cell.fg, cell.flags);
                    let mut bg = resolve_bg(cell.bg);
                    // Reverse video (`man`/`less` header bars, some prompt styling, etc.) —
                    // swap what's ultimately drawn where. Needs concrete colors on both sides
                    // to swap meaningfully, so "use the default" is resolved to this app's
                    // actual default fg/bg first rather than staying `None`.
                    if cell.flags.contains(Flags::INVERSE) {
                        let concrete_fg = fg.unwrap_or((208, 208, 208));
                        let concrete_bg = bg.unwrap_or((0, 0, 0));
                        fg = Some(concrete_bg);
                        bg = Some(concrete_fg);
                    }
                    if let Some(bg) = bg {
                        background_cells.push((row_idx, col_idx, bg));
                    }
                    (cell.c, fg)
                };
                if color != cur_color {
                    if !cur_text.is_empty() {
                        spans.push((std::mem::take(&mut cur_text), cur_color));
                    }
                    cur_color = color;
                }
                cur_text.push(ch);

                if let Some(range) = &selection_range {
                    if range.contains(cell.point) {
                        selection_cells.push((row_idx, col_idx));
                    }
                }
            }
            cur_text.push('\n');
        }
        if !cur_text.is_empty() {
            spans.push((cur_text, cur_color));
        }
        Frame { spans, selection_cells, background_cells }
    }
}

/// One tile's worth of input to `TextPipeline::render_all` — see that method's doc comment for
/// why every tile must be batched into one call instead of one `render` call per tile.
pub struct TileRender<'a> {
    pub frame: &'a Frame,
    /// Top-left origin of this tile's *text* (already offset past the render margin),
    /// physical pixels.
    pub left: f32,
    pub top: f32,
    /// This tile's own clip rectangle (physical pixels) — glyphon clips glyphs to this per
    /// `TextArea`, which is what actually keeps one tile's text from bleeding into a
    /// neighboring tile now (previously done with a wgpu-level `set_scissor_rect` per tile,
    /// which doesn't fit the same-frame-single-`prepare()` requirement below).
    pub clip: (i32, i32, i32, i32),
    pub cell_w: f32,
    pub cell_h: f32,
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

    /// Measures the actual monospace glyph advance width for this app's font (Monaco, 14
    /// logical px — matching `render`'s `Metrics`), instead of assuming one. `CELL_W` was
    /// previously just a guessed constant; a guess even slightly *smaller* than the font's
    /// true advance width systematically overestimates how many columns fit in a given pixel
    /// width, which was invisible in Phase 1/2 (single terminal filling the whole window, so
    /// the overflow just ran into unused margin) but became a real, visible bug once Phase 3
    /// added per-tile wgpu scissor clipping — shell prompts (especially right-aligned
    /// segments, which land exactly at the overestimated rightmost column) rendered past
    /// their tile's true edge and were clipped away entirely.
    pub fn measure_cell_width(&mut self) -> f32 {
        let metrics = Metrics::new(14.0, CELL_H);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(&mut self.font_system, Some(1000.0), Some(100.0));
        buffer.set_text(
            &mut self.font_system,
            "M",
            &Attrs::new().family(Family::Name("Monaco")),
            Shaping::Basic,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer
            .layout_runs()
            .next()
            .map(|run| run.line_w)
            .filter(|w| *w > 0.0)
            .unwrap_or(CELL_W)
    }

    /// Draws a border around one tile in the multi-session grid (Phase 3) — otherwise every
    /// session's pane is just an unbroken black rectangle with no visual separation from its
    /// neighbors. `active` picks a brighter accent color for whichever tile currently has
    /// keyboard focus. `x`/`y`/`w`/`h`/`thickness` are physical pixels, matching the tile's
    /// own scissor rect (the caller is expected to have already set that via
    /// `RenderPass::set_scissor_rect`, so this never draws outside the tile).
    #[allow(clippy::too_many_arguments)]
    pub fn render_tile_border(
        &self,
        device: &wgpu::Device,
        pass: &mut wgpu::RenderPass,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        thickness: f32,
        active: bool,
        viewport_w: u32,
        viewport_h: u32,
    ) {
        self.selection.render_border(device, pass, x, y, w, h, thickness, active, viewport_w, viewport_h);
    }

    /// Renders every currently-visible tile's text (left-aligned within its own origin) in one
    /// pass, in a flat monochrome color (no per-cell ANSI colors — see the plan doc for why:
    /// tried and explicitly rejected in favor of a flat black & white look matching iTerm2's
    /// default monochrome profile). Selection highlight rectangles are drawn first so glyphs
    /// render on top of them, matching how every other terminal composites selection.
    ///
    /// All `tiles` **must** be prepared and rendered together in a single `prepare()`/
    /// `render()`/`trim()` cycle, not one cycle per tile (an earlier version of this code did
    /// exactly that, once per tile, in a loop) — `trim()` evicts glyph atlas entries it
    /// considers no longer in use, and calling it after preparing tile 1 but *before* the GPU
    /// has actually executed tile 1's draw call (which doesn't happen until the whole frame's
    /// command buffer is submitted, after every tile has been processed) could evict the very
    /// atlas region tile 1's already-recorded draw call still points to. Confirmed as the
    /// cause of a real bug: with the old per-tile loop, only the last tile processed ever
    /// rendered its text correctly — every earlier tile stayed blank, since its glyph data had
    /// already been trimmed out from under it by the time the GPU actually drew the frame.
    pub fn render_all(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass,
        tiles: &[TileRender],
        scale_factor: f32,
        viewport_w: u32,
        viewport_h: u32,
    ) {
        for t in tiles {
            // Backgrounds first (opaque, sits directly on the frame's black clear), then the
            // translucent selection tint on top of that, then glyphs on top of both.
            if !t.frame.background_cells.is_empty() {
                self.selection.render_backgrounds(
                    device,
                    pass,
                    &t.frame.background_cells,
                    t.left,
                    t.top,
                    t.cell_w,
                    t.cell_h,
                    viewport_w,
                    viewport_h,
                );
            }
            if !t.frame.selection_cells.is_empty() {
                self.selection.render(
                    device,
                    queue,
                    pass,
                    &t.frame.selection_cells,
                    t.left,
                    t.top,
                    t.cell_w,
                    t.cell_h,
                    viewport_w,
                    viewport_h,
                );
            }
        }

        let metrics = Metrics::new(14.0 * scale_factor, CELL_H * scale_factor);
        // The generic Monospace family resolves to whatever the system default is (Menlo/SF
        // Mono on macOS), which is missing about half of the Vietnamese precomposed Latin
        // block (confirmed earlier this session via direct fontconfig inspection) — e.g. "ắ"
        // (U+1EAF) renders as a tofu box. Monaco, also bundled with macOS, has full coverage
        // (confirmed 0/90 missing).
        let base_attrs = Attrs::new().family(Family::Name("Monaco"));
        // iTerm2's default profile: light gray foreground (not pure white — easier on the
        // eyes against pure black than full-contrast white) — used for any span whose color
        // is `None` (the terminal's default, untouched-by-ANSI-codes text color).
        const DEFAULT_FG: TextColor = TextColor::rgb(208, 208, 208);
        let mut default_attrs = base_attrs.clone();
        default_attrs.color_opt = Some(DEFAULT_FG);

        let mut buffers = Vec::with_capacity(tiles.len());
        for t in tiles {
            let mut buffer = Buffer::new(&mut self.font_system, metrics);
            buffer.set_size(&mut self.font_system, Some(viewport_w as f32), Some(viewport_h as f32));
            let spans: Vec<(&str, Attrs)> = t
                .frame
                .spans
                .iter()
                .map(|(text, color)| {
                    let mut attrs = base_attrs.clone();
                    attrs.color_opt =
                        Some(color.map_or(DEFAULT_FG, |(r, g, b)| TextColor::rgb(r, g, b)));
                    (text.as_str(), attrs)
                })
                .collect();
            buffer.set_rich_text(
                &mut self.font_system,
                spans,
                &default_attrs,
                // Basic is far cheaper than Advanced (skips full BiDi/complex-script
                // analysis) and is enough for a terminal grid — precomposed Vietnamese/Latin
                // diacritics etc. still render correctly, they just don't need script
                // reordering.
                Shaping::Basic,
                None,
            );
            buffer.shape_until_scroll(&mut self.font_system, false);
            buffers.push(buffer);
        }

        let text_areas: Vec<TextArea> = tiles
            .iter()
            .zip(buffers.iter())
            .map(|(t, buffer)| {
                let (cl, ct, cr, cb) = t.clip;
                TextArea {
                    buffer,
                    left: t.left,
                    top: t.top,
                    scale: 1.0,
                    bounds: TextBounds { left: cl, top: ct, right: cr, bottom: cb },
                    // iTerm2's default profile: light gray foreground (not pure white —
                    // easier on the eyes against pure black than full-contrast white).
                    default_color: TextColor::rgb(208, 208, 208),
                    custom_glyphs: &[],
                }
            })
            .collect();

        self.text_renderer
            .prepare(
                device,
                queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                text_areas,
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
        // Selection tint: light, semi-transparent gray — lets the glyph drawn on top (in
        // this app's monochrome light-gray-on-black palette) stay legible.
        let color = [1.0, 1.0, 1.0, 0.28];
        let rects: Vec<(f32, f32, f32, f32, [f32; 4])> = cells
            .iter()
            .map(|&(row, col)| {
                (left + col as f32 * cell_w, top + row as f32 * cell_h, cell_w, cell_h, color)
            })
            .collect();
        self.draw_rects(device, pass, &rects, viewport_w, viewport_h);
    }

    /// Draws each cell's actual (fully opaque, unlike selection's translucent tint) background
    /// color — see `Frame::background_cells`'s doc comment for why this exists at all.
    #[allow(clippy::too_many_arguments)]
    fn render_backgrounds(
        &self,
        device: &wgpu::Device,
        pass: &mut wgpu::RenderPass,
        cells: &[(usize, usize, (u8, u8, u8))],
        left: f32,
        top: f32,
        cell_w: f32,
        cell_h: f32,
        viewport_w: u32,
        viewport_h: u32,
    ) {
        let rects: Vec<(f32, f32, f32, f32, [f32; 4])> = cells
            .iter()
            .map(|&(row, col, (r, g, b))| {
                let color = [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0];
                (left + col as f32 * cell_w, top + row as f32 * cell_h, cell_w, cell_h, color)
            })
            .collect();
        self.draw_rects(device, pass, &rects, viewport_w, viewport_h);
    }

    /// Draws a hollow rectangle outline (four thin filled quads, one per edge) around a tile —
    /// used to visually separate sessions in the tiled grid, and to highlight whichever one
    /// currently has keyboard focus (`active`). All pixel-space, physical pixels.
    #[allow(clippy::too_many_arguments)]
    fn render_border(
        &self,
        device: &wgpu::Device,
        pass: &mut wgpu::RenderPass,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        thickness: f32,
        active: bool,
        viewport_w: u32,
        viewport_h: u32,
    ) {
        let color =
            if active { [0.35, 0.55, 1.0, 1.0] } else { [1.0, 1.0, 1.0, 0.3] };
        let rects = [
            // top
            (x, y, w, thickness, color),
            // bottom
            (x, y + h - thickness, w, thickness, color),
            // left
            (x, y, thickness, h, color),
            // right
            (x + w - thickness, y, thickness, h, color),
        ];
        self.draw_rects(device, pass, &rects, viewport_w, viewport_h);
    }

    fn draw_rects(
        &self,
        device: &wgpu::Device,
        pass: &mut wgpu::RenderPass,
        rects: &[(f32, f32, f32, f32, [f32; 4])],
        viewport_w: u32,
        viewport_h: u32,
    ) {
        if rects.is_empty() {
            return;
        }
        let to_ndc = |x: f32, y: f32| -> [f32; 2] {
            [
                (x / viewport_w as f32) * 2.0 - 1.0,
                1.0 - (y / viewport_h as f32) * 2.0,
            ]
        };
        let mut vertices = Vec::with_capacity(rects.len() * 6);
        for &(x0, y0, w, h, color) in rects {
            let x1 = x0 + w;
            let y1 = y0 + h;
            let tl = SelectionVertex { position: to_ndc(x0, y0), color };
            let tr = SelectionVertex { position: to_ndc(x1, y0), color };
            let bl = SelectionVertex { position: to_ndc(x0, y1), color };
            let br = SelectionVertex { position: to_ndc(x1, y1), color };
            vertices.extend_from_slice(&[tl, bl, tr, tr, bl, br]);
        }

        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad-vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, buffer.slice(..));
        pass.draw(0..vertices.len() as u32, 0..1);
    }
}
