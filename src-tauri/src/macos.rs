//! macOS-specific AppKit interop not covered by winit's cross-platform API.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::rc::Retained;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL, NSResponder, NSScreen, NSTextInputContext};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSURL};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::event_loop::EventLoopProxy;

use crate::macos_input_view::TerminalInputView;
use crate::AppEvent;

/// The primary display's height (points), refreshed from `to_screen_rect` — which runs on the
/// main thread as part of the normal redraw path — for `TerminalInputView`'s `ax_screen_rect`
/// to read without touching `NSScreen` itself. `NSScreen::screens` requires a
/// `MainThreadMarker`, and the thread an incoming `NSAccessibility` call actually lands on
/// isn't reliably recognized as "main" by objc2 in this app (confirmed: forcing the check with
/// `new_unchecked` didn't fix the resulting position — the real `+[NSScreen screens]` call
/// itself was the part failing off the true main thread, not just Rust's own marker check).
/// Stored as raw bits in an atomic rather than an `f64` directly since `AtomicF64` isn't in
/// stable `std` — `to_bits`/`from_bits` round-trip losslessly and the atomic ops involved are
/// exactly the same either way.
pub static PRIMARY_SCREEN_HEIGHT_BITS: AtomicU64 = AtomicU64::new(0);

/// Creates the custom `NSTextInputClient` view (Phase 1d — see the plan doc) as a subview of
/// the window's own content view, and immediately hands it first responder so terminal
/// keyboard/IME input starts flowing through it (and not winit's own, confirmed-buggy IME
/// handling, disabled separately via `Window::set_ime_allowed(false)`) from the very start.
///
/// The returned view must be kept alive by the caller for as long as the window exists —
/// dropping it tears down the AppKit view it wraps.
pub fn install_input_view(
    window: &winit::window::Window,
    proxy: EventLoopProxy<AppEvent>,
    ptt_keycode: u16,
    shortcuts: Vec<(String, crate::commands::KeyBinding)>,
) -> Option<Retained<TerminalInputView>> {
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
    let mtm = MainThreadMarker::new()?;
    unsafe {
        let ns_view = handle.ns_view.as_ptr().cast::<objc2_app_kit::NSView>();
        let ns_view: &objc2_app_kit::NSView = &*ns_view;
        let input_view = TerminalInputView::new(mtm, proxy, ptt_keycode, shortcuts);
        ns_view.addSubview(&input_view);
        focus_input_view(&input_view);
        Some(input_view)
    }
}

/// Hands keyboard "first responder" status to `view` (the custom `TerminalInputView`).
///
/// This is distinct from `Window::focus_window()` (which only affects whether the window is
/// the key/main window at the OS level). AppKit tracks first responder — which specific view
/// within the key window actually receives keyboard events — separately. Once the embedded
/// sidebar webview's `WKWebView` becomes first responder (e.g. the user clicks into it),
/// simply reactivating the window does not hand first responder back to `view` on its own;
/// without this call, clicking outside the webview never routes keyboard input back to the
/// terminal.
pub fn focus_input_view(view: &TerminalInputView) {
    let ns_view: &objc2_app_kit::NSView = view.as_ref();
    if let Some(ns_window) = ns_view.window() {
        let responder: &NSResponder = ns_view.as_ref();
        ns_window.makeFirstResponder(Some(responder));
    }
    if let Some(mtm) = MainThreadMarker::new() {
        unsafe {
            if let Some(ctx) = NSTextInputContext::currentInputContext(mtm) {
                ctx.activate();
            }
        }
    }
}

/// Converts a rectangle in this app's own rendering coordinate space (`x`/`y_from_top`/`w`/`h`,
/// physical pixels, origin top-left of the window's content area, y increasing *downward* —
/// matching wgpu/winit convention) into AppKit screen coordinates (origin bottom-left of the
/// primary display, y increasing *upward*, logical points) — what `NSAccessibility` bounds
/// queries expect. Used to tell `TerminalInputView` where the terminal's cursor actually is on
/// screen, purely for `AXBoundsForRangeParameterizedAttribute` (see that view's doc comment).
///
/// `scale` must be the exact same `window.scale_factor()` reading the caller used to compute
/// `x`/`y_from_top`/`w`/`h` in the first place, rather than re-querying it in here — the two
/// calls aren't guaranteed to agree the moment a window has just moved to a different display
/// (mismatched scale between the physical-pixel inputs and the value used to convert them back
/// to points silently produces a wrong-but-plausible-looking rect, which is exactly what an
/// `isOnSomeScreen`-style caller-side sanity check can't tell apart from a genuinely wrong
/// position).
pub fn to_screen_rect(
    window: &winit::window::Window,
    scale: f64,
    x: f64,
    y_from_top: f64,
    w: f64,
    h: f64,
) -> Option<NSRect> {
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
    if let Some(mtm) = MainThreadMarker::new() {
        if let Some(screen) = NSScreen::screens(mtm).first() {
            PRIMARY_SCREEN_HEIGHT_BITS.store(screen.frame().size.height.to_bits(), Ordering::Relaxed);
        }
    }
    unsafe {
        let ns_view = handle.ns_view.as_ptr().cast::<objc2_app_kit::NSView>();
        let ns_view: &objc2_app_kit::NSView = &*ns_view;
        let ns_window = ns_view.window()?;
        let bounds = ns_view.bounds();
        let (lx, ly, lw, lh) = (x / scale, y_from_top / scale, w / scale, h / scale);
        // winit's root content view (what wgpu/Metal renders into) reports `isFlipped == true`
        // — its own `bounds` are ALREADY top-left/y-down, the same convention `ly` (this app's
        // own top-left/y-down rendering space) is already in. Unconditionally flipping to
        // bottom-left/y-up here, as if the view were never flipped, silently double-flips: the
        // point ends up mirrored across the view's vertical center instead of matching where it
        // was actually drawn. Only apply the bottom-up flip for the (non-flipped) case —
        // checked at runtime rather than hardcoded, since `convertRect:toView:`/
        // `convertRectToScreen:` both correctly handle either convention internally as long as
        // what's handed to them actually matches the view's real one.
        let origin_y = if ns_view.isFlipped() { ly } else { bounds.size.height - ly - lh };
        let local_rect = NSRect {
            origin: NSPoint { x: lx, y: origin_y },
            size: NSSize { width: lw, height: lh },
        };
        let window_rect = ns_view.convertRect_toView(local_rect, None);
        Some(ns_window.convertRectToScreen(window_rect))
    }
}

/// Pushes the terminal's caret position straight to `ai-suggest-menubar` over the same Unix
/// socket the zsh plugin uses, instead of relying on it to query us back through
/// `NSAccessibility`. Exists because that query path — despite `to_screen_rect`'s own math
/// being independently confirmed correct — was still sometimes answered with a stale/unflipped
/// value by macOS's own accessibility bridging (across the classic protocol, `NSTextInputClient`,
/// and their interaction, in ways tracing narrowed down but couldn't fully pin to one root
/// cause) — an undocumented, unpredictable layer entirely outside this app's control. This is
/// the exact rect `to_screen_rect` already returns (AppKit screen coordinates, origin bottom-left,
/// y up), sent as-is — no flipping needed, since the receiver knows to use it directly rather
/// than asking AX.
///
/// Fire-and-forget: a one-message-per-connection send, same framing `OverlayServer` already
/// expects from the zsh plugin. Failing silently (menubar app not running, socket not yet
/// created) is correct — this must never add latency or block the render loop over what's
/// purely a positioning nicety.
pub fn send_cursor_position(rect: NSRect) {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let Some(home) = dirs::home_dir() else { return };
    let socket_path = home.join(".cache/ai-suggest/overlay.sock");
    let Ok(mut stream) = UnixStream::connect(&socket_path) else { return };
    let _ = stream.set_write_timeout(Some(Duration::from_millis(50)));
    let message = format!(
        r#"{{"cursorScreenPosition":{{"pid":{pid},"x":{x},"cellBottomY":{bottom},"cellTopY":{top}}}}}"#,
        pid = std::process::id(),
        x = rect.origin.x,
        bottom = rect.origin.y,
        top = rect.origin.y + rect.size.height,
    );
    let _ = stream.write_all(message.as_bytes());
}

/// Returns the path of the file currently on the general pasteboard, if any — i.e. what's there
/// after selecting a file in Finder and pressing Cmd-C.
///
/// This matters because Finder's "copy file" doesn't just put a `public.file-url` reference on
/// the pasteboard: it also declares icon-derived image data under every common image type
/// (TIFF, PNG, JPEG, ...), generated by Icon Services from the file's Quick Look thumbnail —
/// which for a just-taken screenshot may still be the generic per-type placeholder icon rather
/// than a real preview. `arboard::Clipboard::get_image` has no way to tell that representation
/// apart from an actual copied bitmap (e.g. from a screenshot-to-clipboard shortcut) and happily
/// returns it, so a paste of a copied image *file* can silently produce that placeholder instead
/// of the file's real contents. Reading the file URL directly and using the file's own bytes
/// sidesteps the icon representation entirely.
///
/// Some sources (e.g. a just-taken screenshot handed to the pasteboard by the screenshot
/// utility, rather than a Finder "copy file") declare a *file-reference* URL instead of a
/// path-based one — `url::Url::parse` happily accepts it, but the resulting path looks like
/// `/.file/id=6571367.220528064` with no filename or extension. That path is still resolvable
/// by the OS, but Claude Code's terminal-paste heuristic (like iTerm2/VS Code's) keys off a
/// recognizable image extension to decide whether to render `[Image #1]`, so it falls through
/// to showing the raw path as text instead. `-[NSURL filePathURL]` resolves a file-reference URL
/// back to its real, filename-bearing path (a no-op for URLs that are already path-based).
pub fn clipboard_file_url() -> Option<PathBuf> {
    let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
    let url_string = unsafe { pasteboard.stringForType(NSPasteboardTypeFileURL) }?;
    let url = unsafe { NSURL::URLWithString(&url_string) }?;
    let url = unsafe { url.filePathURL() }.unwrap_or(url);
    let path = unsafe { url.path() }?;
    Some(PathBuf::from(path.to_string()))
}
