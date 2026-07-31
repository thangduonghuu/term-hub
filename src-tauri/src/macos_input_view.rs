//! Custom `NSTextInputClient`-conforming `NSView` that fully replaces winit's own macOS
//! keyboard/IME handling for the terminal — see the plan doc's Phase 1c/1d for why: winit's
//! own IME state machine has a confirmed bug where the keystroke immediately following a
//! composition commit can be silently dropped or delivered with wrong/missing data.
//!
//! Modeled on winit's own `view.rs` for the method signatures required by the protocol, but
//! using a much simpler internal model borrowed from WezTerm's approach (WezTerm doesn't use
//! winit on macOS and hand-rolls this too): an IME commit is always treated as one atomic
//! "composed text" event. A terminal has no document to reach back into mid-composition, so
//! `replacementRange` — which every winit/WezTerm implementation surveyed either mishandles
//! or explicitly ignores — genuinely doesn't need to be implemented at all here.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{declare_class, msg_send, msg_send_id, mutability, sel, ClassType, DeclaredClass};
use objc2_app_kit::{
    NSAccessibilityBoundsForRangeParameterizedAttribute, NSAccessibilityFocusedAttribute,
    NSAccessibilityNumberOfCharactersAttribute, NSAccessibilityParentAttribute,
    NSAccessibilityPositionAttribute, NSAccessibilityRoleAttribute, NSAccessibilitySizeAttribute,
    NSAccessibilitySelectedTextRangeAttribute, NSAccessibilityTextAreaRole,
    NSAccessibilityValueAttribute, NSAccessibilityWindowAttribute, NSEvent,
    NSEventModifierFlags, NSTextInputClient, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSAttributedString, NSAttributedStringKey, NSCopying, NSNotFound,
    NSNumber, NSPoint, NSRange, NSRangePointer, NSRect, NSString, NSValue,
};
use winit::event_loop::EventLoopProxy;

use crate::AppEvent;

pub struct TerminalInputViewIvars {
    proxy: EventLoopProxy<AppEvent>,
    marked_text: RefCell<String>,
    // Screen-coordinate (AppKit convention: origin bottom-left of the primary display, y
    // increasing upward) rectangle of the terminal's cursor cell for whichever tile currently
    // has keyboard focus — updated by `lib.rs` on every redraw via `set_caret_rect`. Exists
    // purely to answer `AXBoundsForRangeParameterizedAttribute` queries — see the doc comment
    // on the accessibility methods below for why those matter at all.
    caret_rect: Cell<NSRect>,
}

declare_class!(
    pub struct TerminalInputView;

    unsafe impl ClassType for TerminalInputView {
        type Super = NSView;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "TerminalInputView";
    }

    impl DeclaredClass for TerminalInputView {
        type Ivars = TerminalInputViewIvars;
    }

    unsafe impl TerminalInputView {
        #[method(acceptsFirstResponder)]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[method(keyDown:)]
        fn key_down(&self, event: &NSEvent) {
            // Cmd+C/Cmd+V don't reach `doCommandBySelector:`'s `copy:`/`paste:` via
            // `interpretKeyEvents:` below — that path only covers AppKit's standard *text*
            // key-binding table (arrows, Ctrl-combos, etc.); Cmd-modified shortcuts are
            // conventionally resolved as Edit-menu key equivalents instead, which never
            // fire without an actual menu bar wired up (this app doesn't have one). Handle
            // them directly here instead of depending on that machinery.
            unsafe {
                let flags = event.modifierFlags();
                if flags.contains(NSEventModifierFlags::NSEventModifierFlagCommand) {
                    if let Some(chars) = event.charactersIgnoringModifiers() {
                        // New/close/next/prev-session shortcuts, matching iTerm2's bindings
                        // (this app already presents itself as `TERM_PROGRAM=iTerm.app` — see
                        // `TerminalSession::spawn`'s doc comment). `charactersIgnoringModifiers`
                        // still applies Shift (only Cmd/Ctrl/Option are ignored), so Cmd+Shift+]
                        // arrives here as "}", not "]" — checked below accordingly.
                        match chars.to_string().as_str() {
                            "c" => {
                                let _ = self.ivars().proxy.send_event(AppEvent::Copy);
                                return;
                            }
                            "v" => {
                                let _ = self.ivars().proxy.send_event(AppEvent::Paste);
                                return;
                            }
                            "t" => {
                                let _ = self
                                    .ivars()
                                    .proxy
                                    .send_event(AppEvent::KeyboardShortcut("new-session"));
                                return;
                            }
                            "w" => {
                                let _ = self
                                    .ivars()
                                    .proxy
                                    .send_event(AppEvent::KeyboardShortcut("close-session"));
                                return;
                            }
                            "}" => {
                                let _ = self
                                    .ivars()
                                    .proxy
                                    .send_event(AppEvent::KeyboardShortcut("next-session"));
                                return;
                            }
                            "{" => {
                                let _ = self
                                    .ivars()
                                    .proxy
                                    .send_event(AppEvent::KeyboardShortcut("prev-session"));
                                return;
                            }
                            _ => {}
                        }
                    }
                } else if flags.contains(NSEventModifierFlags::NSEventModifierFlagControl) {
                    // Ctrl+letter (Ctrl+C to interrupt, Ctrl+D for EOF, Ctrl+Z to suspend,
                    // Ctrl+L to clear, Ctrl+U/A/E for shell readline editing, etc.) must
                    // reach the pty as the raw C0 control byte the shell expects —
                    // `interpretKeyEvents:` below is the wrong path for these even though it
                    // *does* process Ctrl-combos (unlike Cmd-combos above): AppKit's default
                    // key-binding table maps several Ctrl+letter combos to macOS-native text
                    // *editing* actions (e.g. Ctrl+A → `moveToBeginningOfLine:`, an emacs-
                    // style binding meant for text fields), which would silently eat them
                    // instead of ever reaching `insertText:`/`doCommandBySelector:` with
                    // something we'd forward — breaking the shell's own (also emacs-style)
                    // readline shortcuts. Bypass that table entirely and send the byte
                    // ourselves.
                    //
                    // Ctrl+R is the one deliberate exception: it's bound app-wide to Open
                    // Folder (see README/goal-doc — chosen over Cmd+O per explicit user
                    // preference), so it no longer reaches the pty at all. Readline's
                    // reverse-i-search (what Ctrl+R used to do here, same bucket as Ctrl+U/A/E
                    // above) is unreachable in every session as a result — a known, accepted
                    // regression, not an oversight.
                    if let Some(chars) = event.charactersIgnoringModifiers() {
                        if chars.to_string() == "r" {
                            let _ = self
                                .ivars()
                                .proxy
                                .send_event(AppEvent::KeyboardShortcut("open-folder"));
                            return;
                        }
                        if let Some(c) = chars.to_string().chars().next() {
                            if let Some(byte) = crate::control_byte(c) {
                                let _ = self.ivars().proxy.send_event(AppEvent::KeyByte(byte));
                                return;
                            }
                        }
                    }
                }

                // Escape, Delete-forward, Home/End, Page Up/Down: AppKit's default key-
                // binding table maps these to text-editing selectors too (`cancelOperation:`,
                // `deleteForward:`, `moveToBeginningOfLine:`, `scrollPageUp:`, etc.), none of
                // which `doCommandBySelector:` below recognizes — they'd otherwise be silently
                // swallowed with no `KeyControl` ever sent. Confirmed as a real bug: an
                // interactive TUI (an AI CLI's chat interface) appeared to "not respond to any
                // key" because Escape specifically — which that kind of program leans on
                // constantly for canceling/navigating — never reached the pty at all. Identify
                // these by raw virtual keycode (stable, documented Mac constants) rather than
                // guessing selector names, same robustness reasoning as the Ctrl-combo case
                // above.
                let seq: Option<&'static str> = match event.keyCode() {
                    0x35 => Some("\x1b"),     // Escape
                    0x75 => Some("\x1b[3~"),  // Forward Delete
                    0x73 => Some("\x1b[H"),   // Home
                    0x77 => Some("\x1b[F"),   // End
                    0x74 => Some("\x1b[5~"),  // Page Up
                    0x79 => Some("\x1b[6~"),  // Page Down
                    _ => None,
                };
                if let Some(seq) = seq {
                    let _ = self.ivars().proxy.send_event(AppEvent::KeyControl(seq));
                    return;
                }
            }
            let array = NSArray::from_slice(&[event]);
            unsafe { self.interpretKeyEvents(&array) };
        }

        #[method(keyUp:)]
        fn key_up(&self, _event: &NSEvent) {}

        // Just enough of the classic (pre-10.10, string-keyed) NSAccessibility protocol —
        // still the one custom `NSView`s implement — to answer "where is the text caret on
        // screen". Confirmed as a real, concrete need, not speculative: a CLI tool's desktop
        // companion app positions its inline-suggestion popup by querying exactly this via
        // the Accessibility API, and without any conformance here there's nothing for macOS
        // to query at all — every attempt failed with `accessibility error -25205`
        // ("can't complete") and the popup silently never appeared. This is deliberately
        // minimal (real text-editing semantics like actual character offsets aren't
        // implemented — `caret_rect` is just "wherever the terminal's cursor currently is",
        // answered for any range/position asked about) rather than a full accessibility
        // tree; broader screen-reader support would need considerably more than this.
        #[method(accessibilityIsIgnored)]
        fn accessibility_is_ignored(&self) -> bool {
            false
        }

        #[method_id(accessibilityAttributeNames)]
        fn accessibility_attribute_names(&self) -> Retained<NSArray<NSString>> {
            unsafe {
                NSArray::from_vec(vec![
                    NSAccessibilityRoleAttribute.copy(),
                    NSAccessibilityValueAttribute.copy(),
                    NSAccessibilitySelectedTextRangeAttribute.copy(),
                    NSAccessibilityNumberOfCharactersAttribute.copy(),
                    NSAccessibilityFocusedAttribute.copy(),
                    NSAccessibilityParentAttribute.copy(),
                    NSAccessibilityWindowAttribute.copy(),
                    NSAccessibilityPositionAttribute.copy(),
                    NSAccessibilitySizeAttribute.copy(),
                ])
            }
        }

        #[method_id(accessibilityParameterizedAttributeNames)]
        fn accessibility_parameterized_attribute_names(&self) -> Retained<NSArray<NSString>> {
            unsafe {
                NSArray::from_vec(vec![NSAccessibilityBoundsForRangeParameterizedAttribute.copy()])
            }
        }

        #[method_id(accessibilityAttributeValue:)]
        unsafe fn accessibility_attribute_value(
            &self,
            attribute: &NSString,
        ) -> Option<Retained<AnyObject>> {
            let result: Option<Retained<AnyObject>> = if attribute
                .isEqualToString(NSAccessibilityRoleAttribute)
            {
                Some(Retained::cast(NSAccessibilityTextAreaRole.copy()))
            } else if attribute.isEqualToString(NSAccessibilityValueAttribute) {
                Some(Retained::cast(NSString::from_str("")))
            } else if attribute.isEqualToString(NSAccessibilitySelectedTextRangeAttribute) {
                let range = NSRange { location: 0, length: 0 };
                Some(Retained::cast(NSValue::valueWithRange(range)))
            } else if attribute.isEqualToString(NSAccessibilityNumberOfCharactersAttribute) {
                Some(Retained::cast(NSNumber::numberWithInteger(0)))
            } else if attribute.isEqualToString(NSAccessibilityFocusedAttribute) {
                Some(Retained::cast(NSNumber::numberWithBool(true)))
            } else if attribute.isEqualToString(NSAccessibilityParentAttribute)
                || attribute.isEqualToString(NSAccessibilityWindowAttribute)
            {
                let view: &NSView = self.as_ref();
                view.window().map(|w| Retained::cast(w))
            } else if attribute.isEqualToString(NSAccessibilityPositionAttribute) {
                let rect = self.ax_screen_rect();
                Some(Retained::cast(NSValue::valueWithPoint(rect.origin)))
            } else if attribute.isEqualToString(NSAccessibilitySizeAttribute) {
                let rect = self.ax_screen_rect();
                Some(Retained::cast(NSValue::valueWithSize(rect.size)))
            } else {
                None
            };
            result
        }

        #[method_id(accessibilityAttributeValue:forParameter:)]
        unsafe fn accessibility_attribute_value_for_parameter(
            &self,
            attribute: &NSString,
            _parameter: &AnyObject,
        ) -> Option<Retained<AnyObject>> {
            let result: Option<Retained<AnyObject>> = if attribute.to_string() == "AXBoundsForRange" {
                let rect = self.ax_screen_rect();
                Some(Retained::cast(NSValue::valueWithRect(rect)))
            } else {
                None
            };
            result
        }
    }

    #[allow(non_snake_case)]
    unsafe impl NSTextInputClient for TerminalInputView {
        #[method(hasMarkedText)]
        unsafe fn hasMarkedText(&self) -> bool {
            !self.ivars().marked_text.borrow().is_empty()
        }

        #[method(markedRange)]
        unsafe fn markedRange(&self) -> NSRange {
            let len = self.ivars().marked_text.borrow().chars().count();
            if len == 0 {
                NSRange { location: NSNotFound as usize, length: 0 }
            } else {
                NSRange { location: 0, length: len }
            }
        }

        #[method(selectedRange)]
        unsafe fn selectedRange(&self) -> NSRange {
            NSRange { location: NSNotFound as usize, length: 0 }
        }

        #[method(setMarkedText:selectedRange:replacementRange:)]
        unsafe fn setMarkedText_selectedRange_replacementRange(
            &self,
            string: &AnyObject,
            _selected_range: NSRange,
            _replacement_range: NSRange,
        ) {
            let text = ns_object_to_string(string);
            *self.ivars().marked_text.borrow_mut() = text.clone();
            let _ = self.ivars().proxy.send_event(AppEvent::ImePreedit(text));
        }

        #[method(unmarkText)]
        unsafe fn unmarkText(&self) {
            self.ivars().marked_text.borrow_mut().clear();
            let _ = self.ivars().proxy.send_event(AppEvent::ImePreedit(String::new()));
        }

        #[method_id(attributedSubstringForProposedRange:actualRange:)]
        unsafe fn attributedSubstringForProposedRange_actualRange(
            &self,
            _range: NSRange,
            _actual_range: NSRangePointer,
        ) -> Option<Retained<NSAttributedString>> {
            None
        }

        #[method_id(validAttributesForMarkedText)]
        unsafe fn validAttributesForMarkedText(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::new()
        }

        // Where the IME's candidate/conversion window should anchor itself. Vietnamese Telex
        // (the case this was originally built and tested against) never calls this — it has no
        // popup, just inline diacritic composition — so returning `NSRect::ZERO` went
        // unnoticed. CJK input methods (Japanese, Chinese, Korean) rely on exactly this call to
        // position their candidate window at the actual text caret; with it zeroed, the popup
        // renders in the wrong place (typically the screen corner) instead of next to the
        // cursor.
        //
        // Confirmed (via tracing every stage of the pipeline) that this method's return value —
        // not `accessibilityAttributeValue:forParameter:`'s, even though that one is also
        // called and computes the right answer — is what actually reaches `AXUIElementCopy-
        // ParameterizedAttributeValue` for `kAXBoundsForRangeParameterizedAttribute`: macOS
        // apparently prefers a view's `NSTextInputClient` answer over its classic
        // `NSAccessibility` one for text-caret bounds queries when a view implements both,
        // regardless of what the classic protocol method itself returns. So this needs to
        // return the same AX-flipped (top-left/y-down) rect `ax_screen_rect` computes, not the
        // raw AppKit-native (bottom-left/y-up) `caret_rect` — despite `firstRectForCharacterRange:`
        // conventionally wanting AppKit-native "screen coordinates" for real IME popups. Safe to
        // make this trade here: Vietnamese Telex (the only IME actually exercised so far) never
        // calls this at all, so there's no verified-working real-IME behavior to regress —
        // whereas the overlay positioning this rect drives is this file's entire reason for
        // existing.
        #[method(firstRectForCharacterRange:actualRange:)]
        unsafe fn firstRectForCharacterRange_actualRange(
            &self,
            _range: NSRange,
            _actual_range: NSRangePointer,
        ) -> NSRect {
            self.ax_screen_rect()
        }

        #[method(characterIndexForPoint:)]
        unsafe fn characterIndexForPoint(&self, _point: NSPoint) -> usize {
            0
        }

        #[method(insertText:replacementRange:)]
        unsafe fn insertText_replacementRange(&self, string: &AnyObject, _replacement_range: NSRange) {
            let text = ns_object_to_string(string);
            self.ivars().marked_text.borrow_mut().clear();
            let _ = self.ivars().proxy.send_event(AppEvent::ImeCommit(text));
        }

        #[method(doCommandBySelector:)]
        unsafe fn doCommandBySelector(&self, cmd: Sel) {
            let seq: Option<&'static str> = if cmd == sel!(insertNewline:) {
                Some("\r")
            } else if cmd == sel!(deleteBackward:) {
                Some("\x7f")
            } else if cmd == sel!(insertTab:) {
                Some("\t")
            } else if cmd == sel!(moveLeft:) {
                Some("\x1b[D")
            } else if cmd == sel!(moveRight:) {
                Some("\x1b[C")
            } else if cmd == sel!(moveUp:) {
                Some("\x1b[A")
            } else if cmd == sel!(moveDown:) {
                Some("\x1b[B")
            } else {
                None
            };
            if let Some(seq) = seq {
                let _ = self.ivars().proxy.send_event(AppEvent::KeyControl(seq));
            } else if cmd == sel!(copy:) {
                let _ = self.ivars().proxy.send_event(AppEvent::Copy);
            } else if cmd == sel!(paste:) {
                let _ = self.ivars().proxy.send_event(AppEvent::Paste);
            }
        }
    }
);

/// `setMarkedText:`/`insertText:` can hand us either a plain `NSString` or an
/// `NSAttributedString` (marked text sometimes carries underline-style attributes) — Apple's
/// docs guarantee it's always one of exactly those two, so checking for the (rarer)
/// attributed case and otherwise assuming `NSString` is exhaustive, without needing a
/// generic downcast facility (objc2 0.5 doesn't expose one for arbitrary `AnyObject`s).
fn ns_object_to_string(obj: &AnyObject) -> String {
    unsafe {
        let is_attributed: bool = msg_send![obj, isKindOfClass: NSAttributedString::class()];
        if is_attributed {
            let attr = &*(obj as *const AnyObject as *const NSAttributedString);
            attr.string().to_string()
        } else {
            let s = &*(obj as *const AnyObject as *const NSString);
            s.to_string()
        }
    }
}

impl TerminalInputView {
    pub fn new(mtm: MainThreadMarker, proxy: EventLoopProxy<AppEvent>) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(TerminalInputViewIvars {
            proxy,
            marked_text: RefCell::new(String::new()),
            caret_rect: Cell::new(NSRect::ZERO),
        });
        unsafe { msg_send_id![super(this), initWithFrame: NSRect::ZERO] }
    }

    /// Updates where the terminal's cursor currently is, in AppKit screen coordinates (origin
    /// bottom-left of the primary display) — called from `lib.rs` on every redraw. Purely for
    /// `NSAccessibility` queries (see the accessibility methods above); doesn't affect
    /// anything drawn on screen.
    pub fn set_caret_rect(&self, rect: NSRect) {
        self.ivars().caret_rect.set(rect);
    }

    /// `caret_rect` in the coordinate convention `NSAccessibility` clients (`AXUIElementCopy-
    /// AttributeValue` etc.) actually expect: origin top-left of the *primary* display
    /// (`NSScreen.screens[0]`, the one with the menu bar), y increasing *downward*. This is a
    /// real, well-documented quirk, not a guess: it's the one AX API surface that has always
    /// used a flipped convention relative to every other AppKit screen coordinate — apps that
    /// hand-implement the classic `NSAccessibility` protocol (as this view does, rather than
    /// getting it for free from a standard control) are responsible for doing that flip
    /// themselves; AppKit doesn't do it automatically just because you answered with a plain
    /// `NSValue`. Missing this exact flip was confirmed as a real, concrete bug: it left the X
    /// coordinate (unaffected by a Y-only flip) correctly tracking the live cursor column, while
    /// the Y coordinate came out mirrored — an AX client asking "how far down is the cursor"
    /// got back "how far up" instead, placing anything positioned against it (e.g. a CLI tool's
    /// suggestion popup — see the accessibility methods' doc comment) far from the actual
    /// on-screen row. `firstRectForCharacterRange:` deliberately does NOT use this — that's an
    /// `NSTextInputClient` method, which (like the rest of AppKit) wants the normal
    /// bottom-left/y-up convention `caret_rect` is already stored in.
    fn ax_screen_rect(&self) -> NSRect {
        let rect = self.ivars().caret_rect.get();
        // Reads a value `crate::macos::to_screen_rect` refreshes on every redraw — see that
        // static's own doc comment for why: calling `NSScreen::screens` directly from *here*
        // consistently failed (confirmed by tracing both a panicking and a `new_unchecked`
        // `MainThreadMarker`, neither fixed it), because whatever thread the AX server actually
        // delivers this callback on isn't the one `+[NSScreen screens]` itself requires, and
        // that failure happens below Rust — not something a Rust-side panic guard can catch.
        // Reading a plain atomic here needs no thread affinity at all.
        let screen_height = f64::from_bits(
            crate::macos::PRIMARY_SCREEN_HEIGHT_BITS.load(std::sync::atomic::Ordering::Relaxed),
        );
        NSRect {
            origin: NSPoint {
                x: rect.origin.x,
                y: screen_height - rect.origin.y - rect.size.height,
            },
            size: rect.size,
        }
    }
}
