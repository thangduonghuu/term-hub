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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as TermEvent, EventListener, OnResize, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorShape, CursorStyle, Processor, StdSyncHandler,
};
use glyphon::{
    fontdb, Attrs, Buffer, Cache, Color as TextColor, ContentType, CustomGlyph, CustomGlyphId,
    Family, Font, FontSystem, Metrics, RasterizeCustomGlyphRequest, RasterizedCustomGlyph,
    Resolution, Shaping, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;
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

/// The terminal cursor's on-screen shape (from `CSI q`/DECSCUSR, or this app's default — see
/// `TerminalSession::spawn`'s `default_cursor_style`). Drawn as a solid GPU rectangle (see
/// `SelectionPipeline::render_cursor`), not a font glyph — an earlier version spliced a
/// substitute character ('█'/'_'/'│') into the cell stream and let it render like any other
/// glyph, which broke once glyph quads were sized to each character's own tight ink bounding
/// box (see `TextPipeline::render_all`'s doc comment): Monaco's block-drawing glyphs aren't
/// necessarily anchored to fill a full monospace cell, so the cursor could end up smaller than
/// the cell or offset from where a cursor actually needs to sit. A plain rectangle has no such
/// font-dependent sizing quirk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorKind {
    Block,
    Underline,
    Beam,
}

/// A snapshot of what to draw for one frame: every non-blank cell's glyph (with preedit already
/// spliced in) as `(row, col, char, fg color)` — `None` color meaning "the default" — plus the
/// display-space cell currently under the cursor (if visible), the cells under the selection
/// (drawn as highlight rectangles by `SelectionPipeline` *underneath* the text), and cells with
/// a non-default background. Deliberately per-cell rather than coalesced into same-color runs:
/// each cell is drawn as an independently-colored/-positioned custom glyph (see
/// `TextPipeline`'s doc comment), so there's no shaping step left that would benefit from
/// run-coalescing.
#[derive(Clone)]
pub struct Frame {
    pub cells: Vec<(usize, usize, char, Option<(u8, u8, u8)>)>,
    /// The cell the cursor currently occupies, if it's visible right now (blink phase,
    /// scrolled-into-history, `CursorShape::Hidden`, etc. can all make this `None` — see
    /// `snapshot`'s doc comment). That cell's real glyph is *not* also present in `cells` — the
    /// cursor rectangle stands in for it, same net effect as the old glyph-substitution
    /// approach this replaced.
    pub cursor: Option<(usize, usize, CursorKind)>,
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
    // Bumped on anything that can change what `snapshot()` would return for this session
    // (pty output, scroll, selection, resize) — lets `lib.rs` cache the last `Frame` per tile
    // and skip re-walking the whole grid in `snapshot()` for tiles that haven't actually
    // changed since last frame, the same way `TextPipeline::render_all` already skips
    // re-shaping unchanged tiles. Doesn't need to be exact, just monotonic and cheap to read.
    generation: Arc<AtomicU64>,
}

impl TerminalSession {
    pub fn spawn(
        id: String,
        cwd: &str,
        shell: &str,
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
        // `TERM_PROGRAM` reports this app's own real identity rather than spoofing `iTerm.app`
        // (an earlier version of this code did, purely so kiro-cli's shell integration — which
        // only activates for a small allowlist of recognized terminals keyed off exactly this
        // variable — would treat a TermHub session as compatible enough to wrap for its
        // inline-suggestion feature). `TermHub` isn't in that allowlist, so that integration no
        // longer activates in any TermHub session; a real fix would mean getting TermHub added
        // to kiro-cli's own allowlist, not lying about what terminal this is.
        let mut env = std::collections::HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        env.insert("TERM_PROGRAM".to_string(), "TermHub".to_string());
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
        // Previously always `None` regardless of the session's own `SessionMeta.shell` — every
        // session silently got `alacritty_terminal`'s own default ($SHELL/COMSPEC) no matter
        // what was actually stored for it. `shell` empty also falls back to that default,
        // rather than trying to spawn a literal empty program path.
        let shell_opt =
            if shell.trim().is_empty() { None } else { Some(tty::Shell::new(shell.to_string(), Vec::new())) };
        let pty_options = tty::Options {
            shell: shell_opt,
            working_directory: Some(cwd.into()),
            drain_on_exit: true,
            env,
            // Windows-only field (`alacritty_terminal::tty::Options::escape_args`) — standard
            // C-runtime argument escaping, the same convention every other Windows program
            // expects its argv to follow.
            #[cfg(target_os = "windows")]
            escape_args: true,
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

        let generation = Arc::new(AtomicU64::new(0));
        let term_for_reader = term.clone();
        let generation_for_reader = generation.clone();
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
                        generation_for_reader.fetch_add(1, Ordering::Relaxed);
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
            // The shell process is gone (clean exit read as `Ok(0)`, or the pty fd errored
            // out from under us) — tell `App` so it can mark this tile dead instead of just
            // leaving its last frame frozen on screen forever with no indication anything
            // happened (Phase 5's exited-session handling).
            let _ = proxy.send_event(AppEvent::SessionExited(id));
        });

        Ok(Self { term, pty_writer, _pty: pty, generation })
    }

    pub fn write(&mut self, s: &str) {
        // Typing while scrolled back into history should jump back to the live bottom, same
        // convention every real terminal follows (iTerm2, Terminal.app, xterm) — otherwise the
        // keystroke lands in the pty same as always, but the cursor stays scrolled out of view
        // (by design, see `snapshot`'s doc comment on why a scrolled-back cursor is hidden),
        // making it look like typing did nothing.
        self.term.lock().unwrap().scroll_display(Scroll::Bottom);
        self.generation.fetch_add(1, Ordering::Relaxed);
        let _ = self.pty_writer.write_all(s.as_bytes());
    }

    /// Like `write`, but for content that came from an OS paste (`AppEvent::Paste`) rather than
    /// a keystroke. Wraps it in `ESC[200~ … ESC[201~` (bracketed paste) when the child program
    /// has asked for that via DECSET `?2004` — `alacritty_terminal` already tracks that request
    /// as `TermMode::BRACKETED_PASTE`, toggled automatically as the pty's own output is parsed,
    /// so no extra state needs to live here.
    ///
    /// This isn't just cosmetic: readline/editor programs (vim, and notably interactive CLIs
    /// like Claude Code) rely on the bracketing to tell a paste apart from typing at all — e.g.
    /// Claude Code's own image-paste support recognizes a *pasted* file path and shows it as
    /// `[Image #1]`, but without the `ESC[200~`/`ESC[201~` wrapper the same bytes arrive
    /// indistinguishable from someone typing the path out character by character, so that
    /// recognition never fires and the raw path is left sitting in the input line instead.
    pub fn paste(&mut self, s: &str) {
        let bracketed = self.term.lock().unwrap().mode().contains(TermMode::BRACKETED_PASTE);
        if !bracketed {
            self.write(s);
            return;
        }
        // A literal end-marker inside the pasted text would otherwise let it prematurely
        // terminate the bracket and have the remainder read back as ordinary keystrokes —
        // stripped the same way real terminals (xterm, iTerm2) do.
        let sanitized = s.replace("\x1b[201~", "");
        self.write(&format!("\x1b[200~{sanitized}\x1b[201~"));
    }

    /// Resizes both the terminal grid and the pty's own notion of its window size (the latter
    /// via `SIGWINCH`, delivered by the kernel once `on_resize` sets the pty's `winsize` —
    /// without it, programs like `vim`/`htop` that query the terminal size on startup or via
    /// `SIGWINCH` keep drawing for the old dimensions after a window resize).
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let dims = Dims { cols, lines: rows };
        self.term.lock().unwrap().resize(dims);
        self.generation.fetch_add(1, Ordering::Relaxed);
        let window_size = WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: CELL_W as u16,
            cell_height: CELL_H as u16,
        };
        self._pty.on_resize(window_size);
    }

    /// Routes a wheel-scroll event to whichever of three destinations the running program has
    /// asked for, mirroring the same three-way priority real terminals (xterm, Alacritty,
    /// iTerm2) use:
    ///
    /// 1. Mouse tracking active (`?1000`/`?1002`/`?1003` — e.g. Claude Code's own transcript
    ///    view, `htop`, `fzf --mouse`): these programs live on the alt screen with no
    ///    scrollback of their own and explicitly opted into receiving raw wheel events, so
    ///    each notch is reported as an SGR button-64/65 click (`CSI < btn ; col ; row M`) per
    ///    the `?1006` extension they also enable. Without this, wheel input simply vanishes —
    ///    there's no local scrollback to fall back to (see case 3) because the alt screen
    ///    carries none, which is exactly the "can't scroll in Claude Code" symptom this fixes.
    /// 2. No mouse tracking, but still on the alt screen (e.g. plain `less`, `vim`): translated
    ///    to Up/Down keypresses, which is how these programs page through content when they
    ///    haven't asked for mouse events either.
    /// 3. Otherwise (a plain shell prompt): scrolls `termhub`'s own scrollback locally — the
    ///    original behavior, still correct here since the primary screen's grid is the one
    ///    that actually holds history.
    ///
    /// `delta` is signed lines (positive = toward history/up, matching `Scroll::Delta`'s
    /// convention); `col`/`row` are the 0-based cell the pointer is over, only used by the
    /// mouse-report path.
    pub fn wheel(&mut self, delta: i32, col: usize, row: i32) {
        let mode = *self.term.lock().unwrap().mode();
        if mode.intersects(TermMode::MOUSE_MODE) {
            let btn: u8 = if delta > 0 { 64 } else { 65 };
            let report = format!("\x1b[<{btn};{};{}M", col + 1, row.max(0) + 1);
            let mut bytes = Vec::with_capacity(report.len() * delta.unsigned_abs() as usize);
            for _ in 0..delta.unsigned_abs() {
                bytes.extend_from_slice(report.as_bytes());
            }
            let _ = self.pty_writer.write_all(&bytes);
        } else if mode.contains(TermMode::ALT_SCREEN) {
            let seq: &[u8] = if delta > 0 { b"\x1b[A" } else { b"\x1b[B" };
            let mut bytes = Vec::with_capacity(seq.len() * delta.unsigned_abs() as usize);
            for _ in 0..delta.unsigned_abs() {
                bytes.extend_from_slice(seq);
            }
            let _ = self.pty_writer.write_all(&bytes);
        } else {
            self.term.lock().unwrap().scroll_display(Scroll::Delta(delta));
        }
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Monotonic counter, bumped on anything that can change what `snapshot()` returns for
    /// this session. Cheap to read (single atomic load) — lets callers detect "nothing changed
    /// since last time" without paying for an actual `snapshot()` grid walk to find out.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Starts a new selection at the given display-space cell (`row` is 0 at the top of what's
    /// currently visible, negative/positive scroll doesn't change that — the conversion to
    /// `alacritty_terminal`'s absolute grid `Line` happens here via `display_offset`).
    pub fn start_selection(&mut self, col: usize, row: i32) {
        let mut term = self.term.lock().unwrap();
        let offset = term.grid().display_offset() as i32;
        let point = Point::new(Line(row - offset), Column(col));
        term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
        drop(term);
        self.generation.fetch_add(1, Ordering::Relaxed);
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
        drop(term);
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    pub fn clear_selection(&mut self) {
        self.term.lock().unwrap().selection = None;
        self.generation.fetch_add(1, Ordering::Relaxed);
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
        let cursor_kind = match cursor_style.shape {
            CursorShape::Block | CursorShape::HollowBlock => Some(CursorKind::Block),
            CursorShape::Underline => Some(CursorKind::Underline),
            CursorShape::Beam => Some(CursorKind::Beam),
            CursorShape::Hidden => None,
        };
        let show_cursor =
            !scrolled_back && (cursor_visible || !cursor_style.blinking) && cursor_kind.is_some();

        let selection_range = term.selection.as_ref().and_then(|s| s.to_range(&term));

        let mut cells = Vec::new();
        let mut cursor = None;
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
                if in_preedit {
                    // In-progress IME composition text is an overlay too, but real *content*
                    // (unlike the cursor) — still drawn as a glyph, always in the default color
                    // rather than inheriting whatever the underlying cell's color happened to
                    // be.
                    let ch = preedit_chars[col_idx - cursor_col];
                    if ch != ' ' && ch != '\0' {
                        cells.push((row_idx, col_idx, ch, None));
                    }
                } else if is_cursor {
                    // The cursor rectangle (drawn separately, see `CursorKind`) stands in for
                    // this cell's glyph entirely — same net effect the old glyph-substitution
                    // approach had, just not dependent on a font's own block/underline/beam
                    // character design.
                    cursor = Some((row_idx, col_idx, cursor_kind.expect("show_cursor implies Some")));
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
                    // A blank cell draws nothing regardless of color — skip it rather than
                    // handing a whitespace glyph through the rasterizer.
                    if cell.c != ' ' && cell.c != '\0' {
                        cells.push((row_idx, col_idx, cell.c, fg));
                    }
                }

                if let Some(range) = &selection_range {
                    if range.contains(cell.point) {
                        selection_cells.push((row_idx, col_idx));
                    }
                }
            }
        }
        Frame { cells, cursor, selection_cells, background_cells }
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

/// One character rasterized once (via `swash`, see `TextPipeline::rasterize`) at one pixel
/// size — an alpha-only mask, deliberately colorless; `render_all` applies the actual
/// foreground color per glyph *instance* via `CustomGlyph::color`, not here, so the same
/// character drawn in any number of different ANSI colors is still only ever rasterized once.
struct CachedGlyph {
    /// Indexes `TextPipeline::glyph_data` for the actual mask bytes.
    id: CustomGlyphId,
    width: u16,
    height: u16,
    /// Offset from the glyph's pen position to the top-left of its bitmap — swash's
    /// `Placement::left`/`-top`, same convention cosmic-text's own swash-backed rendering uses.
    bearing_left: f32,
    bearing_top: f32,
}

pub struct TextPipeline {
    font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    viewport: Viewport,
    selection: SelectionPipeline,
    /// Permanently empty and never reshaped — `TextArea` requires a `buffer`, but all actual
    /// glyph drawing here goes through `TextArea::custom_glyphs` instead (see `render_all`'s
    /// doc comment), so this exists purely to satisfy the field.
    empty_buffer: Buffer,
    /// Resolved once — every glyph this app ever draws is looked up against these fonts in
    /// order, so there's no reason to re-resolve family names on every rasterization. Index 0
    /// is Monaco, the primary monospace face (and the one `measure_cell_width`/cell-metrics
    /// calculations key off of); the rest are fallbacks tried only when Monaco lacks a glyph —
    /// Nerd Font icons (prompt segment glyphs like folder/git icons), Apple Symbols (extended
    /// box-drawing/misc symbols), and Apple Color Emoji, none of which Monaco covers. Without
    /// this chain, `rasterize` returned `None` for any such character and the cell rendered as
    /// a bare background-color rectangle with no glyph on top — exactly what shows up as
    /// "broken" powerline/nerd-font prompt segments.
    fonts: Vec<Arc<Font>>,
    scale_cx: ScaleContext,
    /// Keyed by (character, pixel font size as bits) — the size half only ever changes if
    /// `scale_factor` does (the window moving to a different-DPI display), so in practice this
    /// caches by character alone. `None` means this font has no glyph for that character
    /// (space, `.notdef`, etc.) — cached too, so a missing-glyph lookup isn't retried forever.
    glyph_cache: std::collections::HashMap<(char, u32, u32, u32), Option<CachedGlyph>>,
    /// Raw alpha-mask bytes for each cached glyph, indexed by `CachedGlyph::id`. Read back by
    /// the `rasterize_custom_glyph` callback `render_all` hands to glyphon, which glyphon calls
    /// at most once per distinct (id, size) the first time its *own* GPU atlas needs it — this
    /// Vec is just a cheap way to answer that without re-invoking `swash`.
    glyph_data: Vec<Vec<u8>>,
    next_glyph_id: CustomGlyphId,
}

impl TextPipeline {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(device);
        let mut viewport = Viewport::new(device, &cache);
        viewport.update(queue, Resolution { width, height });
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        let selection = SelectionPipeline::new(device, format);

        let mut empty_buffer = Buffer::new(&mut font_system, Metrics::new(14.0, CELL_H));
        empty_buffer.set_text(&mut font_system, "", &Attrs::new(), Shaping::Basic);
        empty_buffer.shape_until_scroll(&mut font_system, false);

        let monaco_id = font_system
            .db()
            .query(&fontdb::Query {
                families: &[fontdb::Family::Name("Monaco")],
                ..Default::default()
            })
            .expect("Monaco not found on this system");
        let monaco = font_system.get_font(monaco_id).expect("resolved font id vanished from the cache");

        // Fallback families tried, in order, only when Monaco has no glyph for a character.
        // Any of these can legitimately be absent (they're not bundled with the OS), so
        // resolution failures are skipped rather than treated as fatal. Deliberately excludes
        // color-glyph fonts (e.g. Apple Color Emoji): `rasterize` below renders everything
        // through a single-channel `Format::Alpha` pass, and a color/bitmap glyph run through
        // that comes out as an opaque blob tinted by the cell's foreground color — a solid
        // colored block, which is worse than the blank space a missing glyph used to leave.
        let fallback_families = [
            "Symbols Nerd Font Mono",
            "MesloLGS Nerd Font Mono",
            "Apple Symbols",
        ];
        let mut fonts = vec![monaco];
        for family in fallback_families {
            if let Some(id) = font_system.db().query(&fontdb::Query {
                families: &[fontdb::Family::Name(family)],
                ..Default::default()
            }) {
                if let Some(font) = font_system.get_font(id) {
                    fonts.push(font);
                }
            }
        }

        Self {
            font_system,
            swash_cache,
            atlas,
            text_renderer,
            viewport,
            selection,
            empty_buffer,
            fonts,
            scale_cx: ScaleContext::new(),
            glyph_cache: std::collections::HashMap::new(),
            glyph_data: Vec::new(),
            next_glyph_id: 0,
        }
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
    /// neighbors. `active` picks `accent` (the user's configured accent color — see `App.
    /// accent_color`'s doc comment in lib.rs) for whichever tile currently has keyboard focus;
    /// `exited` (Phase 5) overrides that with a dim red to flag a dead shell process, since
    /// otherwise a tile whose pty died just freezes with no visual difference from a live idle
    /// one. `radius` rounds the tile's corners (see `SelectionPipeline::render_border`'s doc
    /// comment) — pass `0.0` for plain square corners. `x`/`y`/`w`/`h`/`thickness`/`radius` are
    /// physical pixels. Must run after this tile's background/glyphs/cursor are already drawn
    /// (see `RedrawRequested`'s draw order) — rounding works by painting over whatever's
    /// underneath at each corner, same idea as the straight edges painting over it along the
    /// sides.
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
        radius: f32,
        active: bool,
        exited: bool,
        accent: [f32; 4],
        viewport_w: u32,
        viewport_h: u32,
    ) {
        self.selection.render_border(
            device, pass, x, y, w, h, thickness, radius, active, exited, accent, viewport_w,
            viewport_h,
        );
    }

    /// Renders every currently-visible tile's text (left-aligned within its own origin), plus
    /// its background fills and selection highlight underneath. Foreground text goes through
    /// `TextArea::custom_glyphs` — a manually-positioned glyph per non-blank cell — instead of
    /// cosmic-text's usual shaped-`Buffer` path (`t.buffer` below is always `self.empty_buffer`,
    /// permanently empty): this app already knows every glyph's exact monospace-grid cell from
    /// the terminal itself, so there's nothing shaping (line breaking, BiDi, per-run font
    /// resolution) would add. This replaced an earlier version that ran each frame's colored
    /// text through `cosmic_text::Buffer::set_rich_text` with one `Attrs` span per foreground-
    /// color change: profiled at ~120ms/frame on heavily ANSI-colored content (~290 tiny
    /// same-font color runs, e.g. syntax-highlighted output), because cosmic-text 0.14's
    /// `Shaping::Basic` path re-resolves the font's charmap/metrics from scratch for *every*
    /// run rather than once per call. Routing through `custom_glyphs` instead means each
    /// distinct *character* — not each colored run of one — is rasterized via `swash` at most
    /// once ever (see `glyph_for`/`glyph_cache`) and reused at any size/color/count of on-screen
    /// occurrences; color is applied per glyph *instance* at draw time via `CustomGlyph::color`,
    /// fully decoupled from rasterization.
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
            if let Some((row, col, kind)) = t.frame.cursor {
                self.selection.render_cursor(
                    device, pass, row, col, kind, t.left, t.top, t.cell_w, t.cell_h, viewport_w,
                    viewport_h,
                );
            }
        }

        // iTerm2's default profile: light gray foreground (not pure white — easier on the eyes
        // against pure black than full-contrast white) — used for any cell whose color is
        // `None` (the terminal's default, untouched-by-ANSI-codes text color).
        const DEFAULT_FG_RGB: (u8, u8, u8) = (208, 208, 208);
        let font_size_px = 14.0 * scale_factor;

        // Ascent/descent of this app's primary font (Monaco), in pixels at the current size —
        // resolved fresh each call (one cheap `metrics()` call, not per-glyph) since it only
        // actually changes if `scale_factor` does (the window moving to a different-DPI
        // display). Cell layout stays keyed off Monaco alone even when a glyph is drawn from a
        // fallback font, so row/column spacing never shifts based on which characters appear.
        let font_ref = self.fonts[0].as_swash();
        let m = font_ref.metrics(&[]);
        let units_per_em = m.units_per_em as f32;
        let ascent_px = m.ascent / units_per_em * font_size_px;
        let descent_px = m.descent / units_per_em * font_size_px;

        let mut tile_glyphs: Vec<Vec<CustomGlyph>> = Vec::with_capacity(tiles.len());
        for t in tiles {
            // Matches cosmic-text's own line layout (the `centering_offset` term in its
            // `buffer.rs`): center the font's ascent+descent box within the cell height rather
            // than assuming they're equal, so this lines up with how text was positioned before
            // shaping was routed around.
            let centering_offset = (t.cell_h - (ascent_px + descent_px)) / 2.0;
            let mut glyphs = Vec::with_capacity(t.frame.cells.len());
            for &(row, col, ch, color) in &t.frame.cells {
                let Some(g) = self.glyph_for(ch, font_size_px, t.cell_w, t.cell_h) else { continue };
                // Relative to this tile's `TextArea.left`/`top` (set below to `t.left`/`t.top`)
                // — glyphon adds those itself when placing each `CustomGlyph`
                // (`text_area.left + glyph.left * text_area.scale`, see glyphon's
                // `text_render.rs`). Including `t.left`/`t.top` here too used to double-count
                // them: every glyph landed at `2 * t.left` instead of `t.left`, which (since
                // `t.left` is `SIDEBAR_WIDTH` plus a small margin, not some tiny offset) showed
                // up as a large, constant rightward shift of all *text* specifically — while
                // the background-color quads (a separate, hand-rolled wgpu pipeline that adds
                // `t.left` exactly once) stayed correctly positioned, which is why the colored
                // prompt segments always lined up right after the sidebar but the text drawn
                // over them didn't.
                let pen_x = col as f32 * t.cell_w;
                let baseline_y = row as f32 * t.cell_h + centering_offset + ascent_px;
                // `fills_cell` glyphs (box-drawing/block-elements, Nerd Font icons) were already
                // rasterized to `t.cell_w`x`t.cell_h` (or, for a single-stroke line glyph, just
                // one of those axes — see `rasterize`'s doc comment) in `rasterize` — placed at
                // the cell's own top-left corner plus `g.bearing_left/top`'s centering offset
                // (zero except for a line glyph's un-stretched thin axis) rather than baseline-
                // relative like ordinary text, so the stretched axis still tiles edge-to-edge
                // instead of overlapping or gapping neighboring cells.
                let (left, top) = if fills_cell(ch) {
                    (pen_x + g.bearing_left, row as f32 * t.cell_h + g.bearing_top)
                } else {
                    (pen_x + g.bearing_left, baseline_y - g.bearing_top)
                };
                let (r, gr, b) = color.unwrap_or(DEFAULT_FG_RGB);
                glyphs.push(CustomGlyph {
                    id: g.id,
                    left,
                    top,
                    width: g.width as f32,
                    height: g.height as f32,
                    color: Some(TextColor::rgb(r, gr, b)),
                    snap_to_physical_pixel: true,
                    metadata: 0,
                });
            }
            tile_glyphs.push(glyphs);
        }

        let text_areas: Vec<TextArea> = tiles
            .iter()
            .zip(tile_glyphs.iter())
            .map(|(t, glyphs)| {
                let (cl, ct, cr, cb) = t.clip;
                TextArea {
                    buffer: &self.empty_buffer,
                    left: t.left,
                    top: t.top,
                    scale: 1.0,
                    bounds: TextBounds { left: cl, top: ct, right: cr, bottom: cb },
                    default_color: TextColor::rgb(
                        DEFAULT_FG_RGB.0,
                        DEFAULT_FG_RGB.1,
                        DEFAULT_FG_RGB.2,
                    ),
                    custom_glyphs: glyphs,
                }
            })
            .collect();

        // Borrowed ahead of the call below so the closure only captures this one field, not
        // `self` as a whole — `self.text_renderer`/`&mut self.font_system`/`&mut self.atlas` are
        // borrowed separately as the call's other arguments.
        let glyph_data = &self.glyph_data;
        self.text_renderer
            .prepare_with_custom(
                device,
                queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                text_areas,
                &mut self.swash_cache,
                |req: RasterizeCustomGlyphRequest| {
                    glyph_data.get(req.id as usize).map(|data| RasterizedCustomGlyph {
                        data: data.clone(),
                        content_type: ContentType::Mask,
                    })
                },
            )
            .unwrap();
        self.text_renderer.render(&self.atlas, &self.viewport, pass).unwrap();
        self.atlas.trim();
    }

    /// Looks up (rasterizing and permanently caching on first use) the glyph for one character
    /// at one pixel size. `None` means this font has no usable glyph for it (space, `.notdef`,
    /// a codepoint outside Monaco's coverage, etc.) — cached too, so a missing glyph isn't
    /// re-attempted every time it's encountered again.
    ///
    /// `cell_w_px`/`cell_h_px` only matter for `fills_cell` glyphs, which are rasterized at
    /// exactly that size rather than their font-native size — folded into the cache key for
    /// those so a later resize (which changes cell pixel dimensions) re-rasterizes them instead
    /// of reusing a stale-sized bitmap. Ordinary glyphs key on `(0, 0)` regardless of cell size,
    /// same cache behavior as before this distinction existed.
    fn glyph_for(&mut self, ch: char, font_size_px: f32, cell_w_px: f32, cell_h_px: f32) -> Option<&CachedGlyph> {
        let key = if fills_cell(ch) {
            (ch, font_size_px.to_bits(), cell_w_px.to_bits(), cell_h_px.to_bits())
        } else {
            (ch, font_size_px.to_bits(), 0, 0)
        };
        if !self.glyph_cache.contains_key(&key) {
            let entry = self.rasterize(ch, font_size_px, cell_w_px, cell_h_px);
            self.glyph_cache.insert(key, entry);
        }
        self.glyph_cache.get(&key).unwrap().as_ref()
    }

    /// Same recipe cosmic-text's own `swash`-backed rendering uses internally (source order,
    /// `Format::Alpha`, hinting on) — kept identical so glyphs this app draws via
    /// `custom_glyphs` look the same as if they'd gone through cosmic-text's normal path.
    ///
    /// Tries each font in `self.fonts` in order (Monaco first, then the Nerd Font/Symbols/Emoji
    /// fallbacks) and rasterizes from the first one that actually has a glyph for `ch`. Monaco
    /// alone has no coverage for prompt icon glyphs (Nerd Font private-use codepoints), many box
    /// drawing/misc symbols, or emoji — previously any of those fell straight to `None` here,
    /// which left the cell's background color painted with nothing drawn on top of it (visible
    /// as flat, icon-less colored blocks in things like powerlevel10k/starship prompts).
    fn rasterize(&mut self, ch: char, font_size_px: f32, cell_w_px: f32, cell_h_px: f32) -> Option<CachedGlyph> {
        let (font_ref, glyph_id) = self.fonts.iter().find_map(|font| {
            let font_ref = font.as_swash();
            let glyph_id = font_ref.charmap().map(ch);
            (glyph_id != 0).then_some((font_ref, glyph_id))
        })?;
        let mut scaler = self.scale_cx.builder(font_ref).size(font_size_px).hint(true).build();
        let image = Render::new(&[
            Source::ColorOutline(0),
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::Outline,
        ])
        .format(Format::Alpha)
        .render(&mut scaler, glyph_id)?;
        if image.placement.width == 0 || image.placement.height == 0 {
            return None;
        }
        let id = self.next_glyph_id;
        self.next_glyph_id = self.next_glyph_id.checked_add(1).expect(
            "more distinct (char, size) glyphs drawn in one run than fit in a u16 id — \
             should be practically unreachable for real terminal content",
        );
        // Box-drawing/block-element and Nerd Font icon glyphs are meant to tile exactly across a
        // cell (see `fills_cell`), but the font's own outline for them isn't guaranteed to match
        // this app's measured cell size — resample the rasterized mask to the cell's exact pixel
        // box instead of keeping it at whatever size the font naturally produced. `render_all`
        // positions these at the cell's top-left corner plus this centering bearing (not
        // baseline-relative like ordinary text).
        //
        // A single straight stroke (`line_axis`) only needs *one* axis stretched to the cell's
        // full size — the axis it runs along, so it connects edge-to-edge with the same
        // character in the neighboring cell. The font's native raster is already razor-thin in
        // the perpendicular axis (a hairline rule); stretching that axis too, like every other
        // `fills_cell` glyph needs, balloons it into a solid bar (the bug behind e.g. a CLI's
        // box-drawn input border rendering as thick gray blocks instead of thin rules). Corners/
        // tees/crosses/block-elements/icons keep the full 2-axis stretch (`line_axis` is `None`
        // for those), unchanged from before.
        if fills_cell(ch) {
            let full_w = cell_w_px.round().max(1.0) as u16;
            let full_h = cell_h_px.round().max(1.0) as u16;
            let (dst_w, dst_h) = match line_axis(ch) {
                Some(true) => (full_w, image.placement.height as u16),
                Some(false) => (image.placement.width as u16, full_h),
                None => (full_w, full_h),
            };
            let data = resize_alpha(
                &image.data,
                image.placement.width as u16,
                image.placement.height as u16,
                dst_w,
                dst_h,
            );
            self.glyph_data.push(data);
            let bearing_left = (cell_w_px - dst_w as f32).max(0.0) / 2.0;
            let bearing_top = (cell_h_px - dst_h as f32).max(0.0) / 2.0;
            return Some(CachedGlyph { id, width: dst_w, height: dst_h, bearing_left, bearing_top });
        }
        self.glyph_data.push(image.data);
        Some(CachedGlyph {
            id,
            width: image.placement.width as u16,
            height: image.placement.height as u16,
            bearing_left: image.placement.left as f32,
            bearing_top: image.placement.top as f32,
        })
    }
}

/// Characters designed to tile seamlessly, edge-to-edge, across a terminal cell — box-drawing
/// and block-element glyphs (ASCII-art banners like LazyVim's startup logo lean on these) and
/// Nerd Font icon glyphs (file/git/prompt icons) — whose own font-native ink bounding box isn't
/// guaranteed to exactly fill a monospace cell (Monaco's block glyphs specifically don't, see
/// the cursor-rendering doc comment above `CursorKind`). Drawn at the cell's exact pixel box
/// (see `rasterize`/`render_all`) instead of at their natural font size/bearing, unlike ordinary
/// text where small inter-glyph gaps are normal.
fn fills_cell(ch: char) -> bool {
    matches!(ch as u32,
        0x2500..=0x259F // Box Drawing, Block Elements
        | 0xE000..=0xF8FF // Private Use Area — most Nerd Font icon sets live here
        | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD // Supplementary PUA-A/B — newer Nerd Font glyphs
    )
}

/// `Some(true)`/`Some(false)` for a `fills_cell` box-drawing character that's a single straight
/// stroke running horizontally/vertically with no corner, tee, or cross — light/heavy/double/
/// dashed variants of `─` and `│`. `None` for every other `fills_cell` glyph (corners, tees,
/// crosses, block elements, Nerd Font icons), which don't get the thin-axis treatment `rasterize`
/// gives these — see its doc comment for why.
fn line_axis(ch: char) -> Option<bool> {
    match ch as u32 {
        0x2500 | 0x2501 | 0x2504 | 0x2505 | 0x2508 | 0x2509 | 0x254C | 0x254D | 0x2550 => Some(true),
        0x2502 | 0x2503 | 0x2506 | 0x2507 | 0x250A | 0x250B | 0x254E | 0x254F | 0x2551 => Some(false),
        _ => None,
    }
}

/// Bilinearly resamples a single-channel (alpha mask) glyph bitmap to `(dst_w, dst_h)` — see
/// `fills_cell`/`rasterize` for why some glyphs need this instead of their native raster size.
fn resize_alpha(src: &[u8], src_w: u16, src_h: u16, dst_w: u16, dst_h: u16) -> Vec<u8> {
    let (src_w, src_h, dst_w, dst_h) = (src_w as usize, src_h as usize, dst_w as usize, dst_h as usize);
    if src_w == dst_w && src_h == dst_h {
        return src.to_vec();
    }
    let at = |x: isize, y: isize| -> f32 {
        let x = x.clamp(0, src_w as isize - 1) as usize;
        let y = y.clamp(0, src_h as isize - 1) as usize;
        src[y * src_w + x] as f32
    };
    let mut out = vec![0u8; dst_w * dst_h];
    for dy in 0..dst_h {
        let sy = ((dy as f32 + 0.5) * src_h as f32 / dst_h as f32) - 0.5;
        let y0 = sy.floor();
        let fy = sy - y0;
        let y0 = y0 as isize;
        for dx in 0..dst_w {
            let sx = ((dx as f32 + 0.5) * src_w as f32 / dst_w as f32) - 0.5;
            let x0 = sx.floor();
            let fx = sx - x0;
            let x0 = x0 as isize;
            let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
            let bottom = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
            out[dy * dst_w + dx] = (top * (1.0 - fy) + bottom * fy).round() as u8;
        }
    }
    out
}

/// A minimal solid-quad wgpu pipeline for the selection highlight — glyphon only draws text,
/// so per-cell background rectangles (real terminals render selection as a highlighted
/// background, not just differently colored text) need this tiny separate pipeline. Vertices
/// are built directly in clip space (NDC) on the CPU each frame — simplest possible approach
/// given selection changes are rare (mouse-drag driven) and the cell count is small (a few
/// hundred at most), no need for instancing or a persistent vertex buffer.
struct SelectionPipeline {
    pipeline: wgpu::RenderPipeline,
    corner_pipeline: wgpu::RenderPipeline,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SelectionVertex {
    position: [f32; 2],
    color: [f32; 4],
}

/// One corner-ring quad's vertex — see `render_corner_ring`'s doc comment for the technique.
/// `frag_px`/`center_px` are physical pixels (not NDC): the fragment shader compares them
/// directly, using `position` only to place the vertex on screen.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CornerVertex {
    position: [f32; 2],
    frag_px: [f32; 2],
    center_px: [f32; 2],
    inner_radius: f32,
    outer_radius: f32,
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

/// Paints `color` over the ring `inner_radius <= distance(frag, center) <= outer_radius`
/// (both edges antialiased over ~1px), leaving everything else untouched (alpha 0) — see
/// `render_corner_ring`'s doc comment for what this is used for. A single shape covers every
/// case this app needs: a thin annulus draws a border stroke that actually curves through the
/// corner instead of getting chopped off by a straight edge meeting a mask (the bug the
/// straight-edges-plus-punch version of this had); `inner_radius: 0.0` makes it a filled disk;
/// `outer_radius` pinned far past anything the quad can reach makes it "paint everything beyond
/// `inner_radius`", i.e. a punch — used to clean up whatever's beyond the rounded silhouette
/// (a background fill or glyph that happens to reach into the tile's square corner) back to the
/// window's own background color. `frag_px`/`center_px` are physical pixels, not NDC — the
/// fragment shader compares them directly; `position` only places the vertex on screen. Linear
/// interpolation of `frag_px` across the quad is exact here (not just an approximation) since
/// clip space is a plain orthographic 2D mapping, `w` is always 1.
const CORNER_SHADER: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) frag_px: vec2<f32>,
    @location(2) center_px: vec2<f32>,
    @location(3) inner_radius: f32,
    @location(4) outer_radius: f32,
    @location(5) color: vec4<f32>,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) frag_px: vec2<f32>,
    @location(1) center_px: vec2<f32>,
    @location(2) inner_radius: f32,
    @location(3) outer_radius: f32,
    @location(4) color: vec4<f32>,
};
@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = vec4<f32>(in.position, 0.0, 1.0);
    out.frag_px = in.frag_px;
    out.center_px = in.center_px;
    out.inner_radius = in.inner_radius;
    out.outer_radius = in.outer_radius;
    out.color = in.color;
    return out;
}
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let d = distance(in.frag_px, in.center_px);
    let fade_in = smoothstep(in.inner_radius - 1.0, in.inner_radius + 1.0, d);
    let fade_out = 1.0 - smoothstep(in.outer_radius - 1.0, in.outer_radius + 1.0, d);
    let alpha = fade_in * fade_out;
    return vec4<f32>(in.color.rgb, in.color.a * alpha);
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

        let corner_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("corner-shader"),
            source: wgpu::ShaderSource::Wgsl(CORNER_SHADER.into()),
        });
        let corner_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("corner-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &corner_shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<CornerVertex>() as wgpu::BufferAddress,
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
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: std::mem::size_of::<[f32; 4]>() as wgpu::BufferAddress,
                            shader_location: 2,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: std::mem::size_of::<[f32; 6]>() as wgpu::BufferAddress,
                            shader_location: 3,
                            format: wgpu::VertexFormat::Float32,
                        },
                        wgpu::VertexAttribute {
                            offset: std::mem::size_of::<[f32; 7]>() as wgpu::BufferAddress,
                            shader_location: 4,
                            format: wgpu::VertexFormat::Float32,
                        },
                        wgpu::VertexAttribute {
                            offset: std::mem::size_of::<[f32; 8]>() as wgpu::BufferAddress,
                            shader_location: 5,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &corner_shader,
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

        Self { pipeline, corner_pipeline }
    }

    /// Draws one filled ring (annulus) at each of a tile's four corners — see `CORNER_SHADER`'s
    /// doc comment for the fill test itself (a plain disk or an outward "paint past
    /// `inner_radius`" punch are both just different `inner_radius`/`outer_radius` values of the
    /// same shape). `box_radius` sizes the small quad drawn at each corner (always this tile's
    /// rounding radius, physical px) — kept as its own parameter separate from `inner_radius`/
    /// `outer_radius` because the punch case passes an effectively-infinite `outer_radius`
    /// (paint everything past `inner_radius`, unbounded) while still only needing geometry
    /// covering the corner's own small square, not literally out to that radius.
    ///
    /// This exists as its own primitive (rather than only ever being one straight-edges-plus-
    /// corner-punch helper) because a *thin* border stroke can't be rounded by punching a big
    /// circle out of straight full-length edges the way a filled background can: right at the
    /// corner, a border only a few px thick sits almost entirely *outside* a circle whose radius
    /// is the rounding radius (tens of px), so punching that circle out erases most of the
    /// stroke near the corner instead of curving it — confirmed exactly this way, it read as a
    /// chipped/broken corner rather than a rounded one. A ring drawn at the stroke's own
    /// thickness (`inner_radius = rounding_radius - stroke_thickness`, `outer_radius =
    /// rounding_radius`) is what actually traces a curve; see `render_border`'s doc comment for
    /// how the straight edges are shortened to hand off to this ring at each corner instead of
    /// running into it.
    #[allow(clippy::too_many_arguments)]
    fn render_corner_ring(
        &self,
        device: &wgpu::Device,
        pass: &mut wgpu::RenderPass,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        box_radius: f32,
        inner_radius: f32,
        outer_radius: f32,
        color: [f32; 4],
        viewport_w: u32,
        viewport_h: u32,
    ) {
        let r = box_radius.min(w / 2.0).min(h / 2.0).max(0.0);
        if r < 0.5 {
            return;
        }
        let to_ndc = |px: f32, py: f32| -> [f32; 2] {
            [(px / viewport_w as f32) * 2.0 - 1.0, 1.0 - (py / viewport_h as f32) * 2.0]
        };
        // (quad's own top-left origin, arc center) for each of the 4 corners — the arc center
        // is inset by `r` on both axes from the tile's actual corner point, toward the tile's
        // interior.
        let corners = [
            (x, y, x + r, y + r),                 // top-left
            (x + w - r, y, x + w - r, y + r),     // top-right
            (x, y + h - r, x + r, y + h - r),     // bottom-left
            (x + w - r, y + h - r, x + w - r, y + h - r), // bottom-right
        ];
        let mut vertices = Vec::with_capacity(24);
        for &(qx, qy, cx, cy) in &corners {
            let (x0, y0, x1, y1) = (qx, qy, qx + r, qy + r);
            let mk = |px: f32, py: f32| CornerVertex {
                position: to_ndc(px, py),
                frag_px: [px, py],
                center_px: [cx, cy],
                inner_radius,
                outer_radius,
                color,
            };
            let (tl, tr, bl, br) = (mk(x0, y0), mk(x1, y0), mk(x0, y1), mk(x1, y1));
            vertices.extend_from_slice(&[tl, bl, tr, tr, bl, br]);
        }
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("corner-vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        pass.set_pipeline(&self.corner_pipeline);
        pass.set_vertex_buffer(0, buffer.slice(..));
        pass.draw(0..vertices.len() as u32, 0..1);
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

    /// Draws the terminal cursor as a solid shape — see `CursorKind`'s doc comment for why this
    /// isn't a font glyph. Same light-gray-on-black default foreground the cursor glyph used to
    /// be drawn in.
    #[allow(clippy::too_many_arguments)]
    fn render_cursor(
        &self,
        device: &wgpu::Device,
        pass: &mut wgpu::RenderPass,
        row: usize,
        col: usize,
        kind: CursorKind,
        left: f32,
        top: f32,
        cell_w: f32,
        cell_h: f32,
        viewport_w: u32,
        viewport_h: u32,
    ) {
        // Same light-gray-on-black default foreground the cursor glyph used to be drawn in.
        let color = [208.0 / 255.0, 208.0 / 255.0, 208.0 / 255.0, 1.0];
        let x = left + col as f32 * cell_w;
        let y = top + row as f32 * cell_h;
        let rect = match kind {
            CursorKind::Block => (x, y, cell_w, cell_h, color),
            CursorKind::Underline => {
                let thickness = (cell_h * 0.12).max(1.0);
                (x, y + cell_h - thickness, cell_w, thickness, color)
            }
            CursorKind::Beam => {
                let thickness = (cell_w * 0.15).max(1.0);
                (x, y, thickness, cell_h, color)
            }
        };
        self.draw_rects(device, pass, &[rect], viewport_w, viewport_h);
    }

    /// Draws a hollow rectangle outline around a tile — used to visually separate sessions in
    /// the tiled grid, and to highlight whichever one currently has keyboard focus (`active`).
    /// All pixel-space, physical pixels.
    ///
    /// The focused tile gets `accent` — the user's configured accent color (`App.accent_color`
    /// in lib.rs), shared with the active row in the sidebar and the Quick Open input's focus
    /// ring via the frontend's own `--accent-color` CSS variable, for visual consistency across
    /// the app. A single flat-color line, not a two-layer glow band.
    ///
    /// `radius` rounds the corners. This is *not* "draw the usual full-length straight edges,
    /// then punch a circle out of the corner": a first attempt did exactly that, and it reads as
    /// a chipped/broken corner rather than a rounded one — a thin stroke sits almost entirely
    /// outside a circle whose radius is tens of px, so punching that circle erases most of the
    /// stroke near the corner instead of curving it, and unevenly across the stroke's own
    /// thickness (nothing like a clean arc). Instead each straight edge is shortened by `radius`
    /// at both ends (`border_rects`), and a proper ring — `inner_radius = radius - thickness` to
    /// `outer_radius = radius`, see `render_corner_ring` — fills in the curve at each corner, so
    /// the stroke itself actually traces the rounding instead of being clipped by it. A final
    /// "paint everything past `radius`" punch (`punch_corners`) cleans up whatever's beyond the
    /// rounded silhouette in the tile's own square corner (background fill, a glyph) — since
    /// tiles sit flush against each other with no gap (`tile_rects`), that's always this app's
    /// fixed black background, at both the window's own outer corners and at a shared interior
    /// corner where several tiles' own quarter-punches read together as one small rounded notch.
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
        radius: f32,
        active: bool,
        exited: bool,
        accent: [f32; 4],
        viewport_w: u32,
        viewport_h: u32,
    ) {
        let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
        if exited {
            self.render_flat_border(
                device, pass, x, y, w, h, r, thickness, [0.85, 0.3, 0.25, 0.8], viewport_w,
                viewport_h,
            );
            return;
        }
        if active {
            self.render_flat_border(
                device, pass, x, y, w, h, r, thickness * 1.5, accent, viewport_w, viewport_h,
            );
            return;
        }
        self.render_flat_border(
            device, pass, x, y, w, h, r, thickness, [1.0, 1.0, 1.0, 0.3], viewport_w, viewport_h,
        );
    }

    /// One flat-color rounded border: shortened straight edges plus the corner ring that
    /// completes the curve, then the outer punch that cleans up each corner — see
    /// `render_border`'s doc comment for why it takes all three to round a stroke properly.
    #[allow(clippy::too_many_arguments)]
    fn render_flat_border(
        &self,
        device: &wgpu::Device,
        pass: &mut wgpu::RenderPass,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        thickness: f32,
        color: [f32; 4],
        viewport_w: u32,
        viewport_h: u32,
    ) {
        let rects = Self::border_rects(x, y, w, h, thickness, r, color);
        self.draw_rects(device, pass, &rects, viewport_w, viewport_h);
        if r < 0.5 {
            return;
        }
        self.render_corner_ring(
            device, pass, x, y, w, h, r, (r - thickness).max(0.0), r, color, viewport_w,
            viewport_h,
        );
        self.punch_corners(device, pass, x, y, w, h, r, viewport_w, viewport_h);
    }

    /// Paints this app's fixed black window background over everything past `r` from each
    /// corner's arc center — the cleanup step described in `render_border`'s doc comment. A
    /// no-op when `r` is ~0 (rounding disabled).
    #[allow(clippy::too_many_arguments)]
    fn punch_corners(
        &self,
        device: &wgpu::Device,
        pass: &mut wgpu::RenderPass,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        viewport_w: u32,
        viewport_h: u32,
    ) {
        if r < 0.5 {
            return;
        }
        const BACKGROUND: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
        // Comfortably beyond anything a corner's own small quad can reach (see
        // `render_corner_ring`'s doc comment on `box_radius` vs. `outer_radius`) — stands in for
        // "unbounded" so this ring becomes a plain "paint everything past `r`" punch.
        const FAR: f32 = 1.0e6;
        self.render_corner_ring(
            device, pass, x, y, w, h, r, r, FAR, BACKGROUND, viewport_w, viewport_h,
        );
    }

    /// The four straight edges of a tile's border, each shortened by `r` at both ends so a
    /// `render_corner_ring` call can take over the curve there — see `render_border`'s doc
    /// comment. `r` of `0.0` reduces this to the original full-length, square-cornered edges.
    fn border_rects(
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        thickness: f32,
        r: f32,
        color: [f32; 4],
    ) -> [(f32, f32, f32, f32, [f32; 4]); 4] {
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        let span_w = (w - 2.0 * r).max(0.0);
        let span_h = (h - 2.0 * r).max(0.0);
        [
            // top
            (x + r, y, span_w, thickness, color),
            // bottom
            (x + r, y + h - thickness, span_w, thickness, color),
            // left
            (x, y + r, thickness, span_h, color),
            // right
            (x + w - thickness, y + r, thickness, span_h, color),
        ]
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
