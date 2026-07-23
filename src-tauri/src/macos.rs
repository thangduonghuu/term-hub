//! macOS-specific AppKit interop not covered by winit's cross-platform API.

use objc2::rc::Retained;
use objc2_app_kit::{NSResponder, NSTextInputContext};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::event_loop::EventLoopProxy;

use crate::macos_input_view::TerminalInputView;
use crate::AppEvent;

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
) -> Option<Retained<TerminalInputView>> {
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
    let mtm = MainThreadMarker::new()?;
    unsafe {
        let ns_view = handle.ns_view.as_ptr().cast::<objc2_app_kit::NSView>();
        let ns_view: &objc2_app_kit::NSView = &*ns_view;
        let input_view = TerminalInputView::new(mtm, proxy);
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
pub fn to_screen_rect(
    window: &winit::window::Window,
    x: f64,
    y_from_top: f64,
    w: f64,
    h: f64,
) -> Option<NSRect> {
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
    let scale = window.scale_factor();
    unsafe {
        let ns_view = handle.ns_view.as_ptr().cast::<objc2_app_kit::NSView>();
        let ns_view: &objc2_app_kit::NSView = &*ns_view;
        let ns_window = ns_view.window()?;
        let bounds = ns_view.bounds();
        let (lx, ly, lw, lh) = (x / scale, y_from_top / scale, w / scale, h / scale);
        // Flip from this app's top-left/y-down convention to AppKit's own view-local
        // bottom-left/y-up convention before handing it to `convertRect:toView:`/
        // `convertRectToScreen:`, which both expect (and preserve) that convention.
        let flipped_y = bounds.size.height - ly - lh;
        let local_rect = NSRect {
            origin: NSPoint { x: lx, y: flipped_y },
            size: NSSize { width: lw, height: lh },
        };
        let window_rect = ns_view.convertRect_toView(local_rect, None);
        Some(ns_window.convertRectToScreen(window_rect))
    }
}
