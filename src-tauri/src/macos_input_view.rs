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

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{declare_class, msg_send, msg_send_id, mutability, sel, ClassType, DeclaredClass};
use objc2_app_kit::{NSEvent, NSTextInputClient, NSView};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSAttributedString, NSAttributedStringKey, NSNotFound, NSPoint,
    NSRange, NSRangePointer, NSRect, NSString,
};
use winit::event_loop::EventLoopProxy;

use crate::AppEvent;

pub struct TerminalInputViewIvars {
    proxy: EventLoopProxy<AppEvent>,
    marked_text: RefCell<String>,
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
            let array = NSArray::from_slice(&[event]);
            unsafe { self.interpretKeyEvents(&array) };
        }

        #[method(keyUp:)]
        fn key_up(&self, _event: &NSEvent) {}
    }

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

        #[method(firstRectForCharacterRange:actualRange:)]
        unsafe fn firstRectForCharacterRange_actualRange(
            &self,
            _range: NSRange,
            _actual_range: NSRangePointer,
        ) -> NSRect {
            NSRect::ZERO
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
        });
        unsafe { msg_send_id![super(this), initWithFrame: NSRect::ZERO] }
    }
}
