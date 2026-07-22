//! macOS-specific AppKit interop not covered by winit's cross-platform API.

use objc2_app_kit::{NSResponder, NSTextInputContext};
use objc2_foundation::MainThreadMarker;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// Hands keyboard "first responder" status back to the window's own content view.
///
/// This is distinct from `Window::focus_window()` (which only affects whether the window
/// is the key/main window at the OS level). AppKit tracks first responder — which specific
/// view within the key window actually receives keyboard events — separately. Once the
/// embedded sidebar webview's `WKWebView` becomes first responder (e.g. the user clicks
/// into it), simply reactivating the window does not hand first responder back to the
/// content view on its own; without this call, clicking outside the webview never routes
/// keyboard input back to the terminal.
///
/// Also explicitly re-activates the current `NSTextInputContext` — `makeFirstResponder:`
/// alone was enough for plain key events (`insertText:`-less `keyDown:` handling) but left
/// IME composition (Vietnamese Telex etc., which goes through `NSTextInputClient`'s
/// `setMarkedText:`/`insertText:replacementRange:`) broken, suggesting the input context
/// AppKit normally sets up automatically when a view becomes first responder through a
/// user-initiated event doesn't happen the same way when responder status is reassigned
/// programmatically like this.
pub fn reclaim_first_responder(window: &winit::window::Window) {
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    unsafe {
        let ns_view = handle.ns_view.as_ptr().cast::<objc2_app_kit::NSView>();
        let ns_view: &objc2_app_kit::NSView = &*ns_view;
        if let Some(ns_window) = ns_view.window() {
            let responder: &NSResponder = ns_view.as_ref();
            ns_window.makeFirstResponder(Some(responder));
        }
        if let Some(mtm) = MainThreadMarker::new() {
            if let Some(ctx) = NSTextInputContext::currentInputContext(mtm) {
                ctx.discardMarkedText();
                ctx.activate();
            }
        }
    }
}
