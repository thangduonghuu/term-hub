import { useEffect, useState } from "react";
import { Keyboard, Mic, RotateCcw, TerminalSquare, X } from "lucide-react";
import { api, type KeyBinding } from "../lib/api";

interface Props {
  onClose: () => void;
}

// Browser `KeyboardEvent.code` -> the macOS virtual keycode `macos_input_view.rs` actually
// matches shortcuts against (`NSEvent::keyCode()`, in `key_down`'s dispatch loop and
// `flags_changed` for push-to-talk). Kept as a separate map (rather than, say, asking Rust to
// also ship DOM code strings) because this only exists to interpret what the *browser* reports
// a physical key as — the shared source of truth both sides agree on is the resulting macOS
// keycode number, not this table itself. Covers every key any of this app's shortcuts could
// reasonably be rebound to; arrow keys are deliberately absent (already meaningful to the
// terminal itself — cursor movement — so binding an app action to one would shadow that).
const DOM_CODE_TO_MACOS_KEYCODE: Record<string, number> = {
  KeyA: 0x00, KeyB: 0x0b, KeyC: 0x08, KeyD: 0x02, KeyE: 0x0e, KeyF: 0x03, KeyG: 0x05,
  KeyH: 0x04, KeyI: 0x22, KeyJ: 0x26, KeyK: 0x28, KeyL: 0x25, KeyM: 0x2e, KeyN: 0x2d,
  KeyO: 0x1f, KeyP: 0x23, KeyQ: 0x0c, KeyR: 0x0f, KeyS: 0x01, KeyT: 0x11, KeyU: 0x20,
  KeyV: 0x09, KeyW: 0x0d, KeyX: 0x07, KeyY: 0x10, KeyZ: 0x06,
  Digit1: 0x12, Digit2: 0x13, Digit3: 0x14, Digit4: 0x15, Digit5: 0x17,
  Digit6: 0x16, Digit7: 0x1a, Digit8: 0x1c, Digit9: 0x19, Digit0: 0x1d,
  Minus: 0x1b, Equal: 0x18, BracketLeft: 0x21, BracketRight: 0x1e,
  Quote: 0x27, Semicolon: 0x29, Backslash: 0x2a, Comma: 0x2b, Slash: 0x2c,
  Period: 0x2f, Backquote: 0x32,
  Tab: 0x30, Space: 0x31, Enter: 0x24, Backspace: 0x33, Escape: 0x35,
  F1: 0x7a, F2: 0x78, F3: 0x63, F4: 0x76, F5: 0x60, F6: 0x61, F7: 0x62,
  F8: 0x64, F9: 0x65, F10: 0x6d, F11: 0x67, F12: 0x6f,
  // Modifier keys held alone — voice dictation's push-to-talk key only, not a valid choice for
  // any other shortcut (those all require a "real" key too — see the capture handlers below).
  AltRight: 0x3d, AltLeft: 0x3a, ShiftRight: 0x3c,
  ControlRight: 0x3e, ControlLeft: 0x3b, MetaRight: 0x36,
};

// The reverse direction, for *displaying* a binding that came back from Rust as a keycode
// number — only needs entries for keys a shortcut could actually be bound to (excludes the
// modifier-alone codes above, which are voice dictation's own display, not this one's).
const MACOS_KEYCODE_TO_LABEL: Record<number, string> = Object.fromEntries(
  Object.entries(DOM_CODE_TO_MACOS_KEYCODE)
    .filter(([code]) => !["AltRight", "AltLeft", "ShiftRight", "ControlRight", "ControlLeft", "MetaRight"].includes(code))
    .map(([code, keycode]) => [
      keycode,
      code.startsWith("Key")
        ? code.slice(3)
        : code.startsWith("Digit")
          ? code.slice(5)
          : { Minus: "-", Equal: "=", BracketLeft: "[", BracketRight: "]", Quote: "'", Semicolon: ";",
              Backslash: "\\", Comma: ",", Slash: "/", Period: ".", Backquote: "`", Tab: "Tab",
              Space: "Space", Enter: "Enter", Backspace: "Delete", Escape: "Esc" }[code] ?? code,
    ]),
);

function formatBinding(b: KeyBinding): string {
  let s = "";
  if (b.ctrl) s += "⌃";
  if (b.alt) s += "⌥";
  if (b.shift) s += "⇧";
  if (b.cmd) s += "⌘";
  return s + (MACOS_KEYCODE_TO_LABEL[b.keycode] ?? "?");
}

// One entry per left-nav category — content lives inline in the render below, keyed the same
// way, so adding a new settings category only means adding one entry here plus one `{activeSection === "..." && ...}` block.
const SECTIONS = [
  { id: "general", label: "General", icon: TerminalSquare },
  { id: "keyboard", label: "Keyboard Shortcuts", icon: Keyboard },
  { id: "voice", label: "Voice Dictation", icon: Mic },
] as const;
type SectionId = (typeof SECTIONS)[number]["id"];

export function SettingsPanel({ onClose }: Props) {
  const [activeSection, setActiveSection] = useState<SectionId>("general");
  const [shell, setShell] = useState("");
  const [shellSaved, setShellSaved] = useState(false);
  const [terminalApps, setTerminalApps] = useState<string[]>([]);
  const [preferredApp, setPreferredApp] = useState("");
  const [appSaved, setAppSaved] = useState(false);
  const [pttOptions, setPttOptions] = useState<[number, string][]>([]);
  const [pttKeycode, setPttKeycode] = useState<number | null>(null);
  const [pttSaved, setPttSaved] = useState(false);
  const [pttRecording, setPttRecording] = useState(false);
  const [pttCaptureError, setPttCaptureError] = useState<string | null>(null);
  const [shortcuts, setShortcuts] = useState<[string, string, KeyBinding][]>([]);
  const [recordingAction, setRecordingAction] = useState<string | null>(null);
  const [shortcutCaptureError, setShortcutCaptureError] = useState<string | null>(null);

  useEffect(() => {
    api.getDefaultShell().then((s) => setShell(s ?? ""));
    api.listTerminalApps().then(setTerminalApps);
    api.getPreferredTerminalApp().then((a) => setPreferredApp(a ?? ""));
    api.getVoicePttKeyOptions().then(setPttOptions);
    api.getVoicePttKeycode().then(setPttKeycode);
    api.getShortcuts().then(setShortcuts);
  }, []);

  // While "recording" (after clicking Change), capture the very next physical key press to
  // rebind push-to-talk to it — the same interaction VS Code's own keybinding editor uses.
  // Capture-phase + `stopPropagation` so the keystroke used to set the binding doesn't also do
  // whatever it'd normally do (e.g. Escape closing this whole panel, or a modifier the terminal
  // itself would otherwise react to). Only modifier keys resolve to anything in
  // `DOM_CODE_TO_MACOS_KEYCODE` — everything else (a letter, a held-alone Tab, etc.) is rejected
  // with an inline explanation rather than silently accepted, since only modifier keys can
  // actually work as push-to-talk at all (see `PTT_KEY_OPTIONS`'s doc comment in
  // macos_input_view.rs for the confirmed AppKit bug that rules out anything else).
  useEffect(() => {
    if (!pttRecording) return;
    function onKeyDown(e: KeyboardEvent) {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") {
        setPttRecording(false);
        return;
      }
      const keycode = DOM_CODE_TO_MACOS_KEYCODE[e.code];
      if (keycode === undefined || !pttOptions.some(([code]) => code === keycode)) {
        setPttCaptureError("Only a modifier key held alone works — try Option, Shift, Control, or right Command.");
        return;
      }
      setPttCaptureError(null);
      setPttRecording(false);
      savePttKeycode(keycode);
    }
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [pttRecording, pttOptions]);

  // Same idea as the push-to-talk capture above, but for the ordinary (press-to-trigger, not
  // hold) shortcuts: Copy, Paste, New/Close/Next/Prev session, Open folder. These accept any
  // key `DOM_CODE_TO_MACOS_KEYCODE` knows, but — unlike push-to-talk — always require at least
  // one of Cmd/Ctrl/Option alongside it: a binding like bare "W" (no modifier) would hijack
  // every ordinary keystroke typed at the shell, since these fire on *press* rather than
  // needing a held modifier's own transition the way push-to-talk does.
  useEffect(() => {
    if (!recordingAction) return;
    function onKeyDown(e: KeyboardEvent) {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        setRecordingAction(null);
        return;
      }
      // Still waiting for a "real" key — a bare modifier keydown on its own isn't a complete
      // combination yet.
      if (["Control", "Alt", "Shift", "Meta"].includes(e.key)) return;
      const keycode = DOM_CODE_TO_MACOS_KEYCODE[e.code];
      const hasAnchorModifier = e.metaKey || e.ctrlKey || e.altKey;
      if (keycode === undefined || !hasAnchorModifier) {
        setShortcutCaptureError(
          "Include Cmd, Ctrl, or Option so this doesn't collide with normal typing in the terminal.",
        );
        return;
      }
      setShortcutCaptureError(null);
      // Non-null: the effect returns early above whenever `recordingAction` is null, so it's
      // guaranteed set for the whole lifetime of this particular effect run/closure.
      const action = recordingAction!;
      setRecordingAction(null);
      saveShortcut(action, {
        cmd: e.metaKey,
        ctrl: e.ctrlKey,
        shift: e.shiftKey,
        alt: e.altKey,
        keycode,
      });
    }
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [recordingAction]);

  // Voice Dictation is only ever meaningful where it's actually implemented (macOS, for now —
  // `pttOptions` comes back empty everywhere else) — same "hide the whole section, don't just
  // gray it out" treatment External Terminal already gets when no apps are installed. Bumped
  // out of the nav entirely, not just skipped in content, so there's nothing to click into that
  // would show as empty. Keyboard Shortcuts hides the same way, off `shortcuts` instead.
  const sections = SECTIONS.filter(
    (s) =>
      (s.id !== "voice" || pttOptions.length > 0) && (s.id !== "keyboard" || shortcuts.length > 0),
  );

  async function savePttKeycode(keycode: number) {
    setPttKeycode(keycode);
    await api.setVoicePttKeycode(keycode);
    setPttSaved(true);
    setTimeout(() => setPttSaved(false), 1500);
  }

  async function saveShortcut(action: string, binding: KeyBinding) {
    await api.setShortcut(action, binding);
    api.getShortcuts().then(setShortcuts);
  }

  async function resetShortcutAction(action: string) {
    await api.resetShortcut(action);
    api.getShortcuts().then(setShortcuts);
  }

  async function saveShell() {
    const trimmed = shell.trim();
    if (trimmed) {
      await api.setDefaultShell(trimmed);
    } else {
      await api.clearDefaultShell();
    }
    setShellSaved(true);
    setTimeout(() => setShellSaved(false), 1500);
  }

  async function savePreferredApp(app: string) {
    setPreferredApp(app);
    if (app) {
      await api.setPreferredTerminalApp(app);
      setAppSaved(true);
      setTimeout(() => setAppSaved(false), 1500);
    }
  }

  return (
    <div className="usage-overlay" onClick={onClose}>
      <div className="settings-panel" onClick={(e) => e.stopPropagation()}>
        <div className="usage-header">
          <h2>Settings</h2>
          <button className="usage-close-btn" onClick={onClose} title="Close">
            <X size={16} />
          </button>
        </div>

        <div className="settings-body">
          <nav className="settings-nav">
            {sections.map(({ id, label, icon: Icon }) => (
              <button
                key={id}
                className={`settings-nav-item ${activeSection === id ? "active" : ""}`}
                onClick={() => setActiveSection(id)}
              >
                <Icon size={14} />
                {label}
              </button>
            ))}
          </nav>

          <div className="settings-content">
            {activeSection === "general" && (
              <>
                <div className="usage-section">
                  <div className="usage-section-header">
                    <h3>Default shell</h3>
                  </div>
                  <p className="claude-key-note">
                    Overrides which shell new sessions spawn (e.g. <code>/bin/zsh</code>,{" "}
                    <code>/opt/homebrew/bin/fish</code>). Leave blank to use{" "}
                    <code>$SHELL</code>/<code>COMSPEC</code> instead. Only affects sessions
                    created after saving — already-open sessions keep whatever shell they
                    started with.
                  </p>
                  <div className="claude-key-input-row">
                    <input
                      type="text"
                      placeholder="System default"
                      value={shell}
                      onChange={(e) => setShell(e.currentTarget.value)}
                      onKeyDown={(e) => e.key === "Enter" && saveShell()}
                    />
                    <button onClick={saveShell}>{shellSaved ? "Saved" : "Save"}</button>
                  </div>
                </div>

                <div className="usage-section">
                  <div className="usage-section-header">
                    <h3>External terminal</h3>
                  </div>
                  {terminalApps.length === 0 ? (
                    <p className="claude-key-note">
                      No supported terminal apps found on this machine — "open in external
                      terminal" (the <code>⤢</code> button next to each session) won't have
                      anywhere to open.
                    </p>
                  ) : (
                    <>
                      <p className="claude-key-note">
                        Which app the <code>⤢</code> button next to each session opens its
                        folder in, alongside the built-in terminal.
                      </p>
                      <select
                        value={preferredApp}
                        onChange={(e) => savePreferredApp(e.currentTarget.value)}
                      >
                        <option value="" disabled>
                          Choose an app…
                        </option>
                        {terminalApps.map((app) => (
                          <option key={app} value={app}>
                            {app}
                          </option>
                        ))}
                      </select>
                      {appSaved && <span className="settings-saved-hint">Saved</span>}
                    </>
                  )}
                </div>
              </>
            )}

            {activeSection === "keyboard" && shortcuts.length > 0 && (
              <div className="usage-section">
                <div className="usage-section-header">
                  <h3>Keyboard Shortcuts</h3>
                </div>
                <p className="claude-key-note">
                  Click Change and press the key combination you want. Must include Cmd, Ctrl,
                  or Option — a shortcut with no modifier would hijack normal typing at the
                  shell.
                </p>
                <div className="shortcut-list">
                  {shortcuts.map(([id, label, binding]) => (
                    <div key={id} className="shortcut-row">
                      <span className="shortcut-label">{label}</span>
                      <span className={`shortcut-keys ${recordingAction === id ? "recording" : ""}`}>
                        {recordingAction === id ? "Press keys… (Esc to cancel)" : formatBinding(binding)}
                      </span>
                      {recordingAction !== id && (
                        <>
                          <button
                            onClick={() => {
                              setRecordingAction(id);
                              setShortcutCaptureError(null);
                            }}
                          >
                            Change
                          </button>
                          <button
                            className="shortcut-reset"
                            onClick={() => resetShortcutAction(id)}
                            title="Reset to default"
                          >
                            <RotateCcw size={13} />
                          </button>
                        </>
                      )}
                    </div>
                  ))}
                </div>
                {shortcutCaptureError && <p className="ptt-capture-error">{shortcutCaptureError}</p>}
              </div>
            )}

            {activeSection === "voice" && pttOptions.length > 0 && (
              <div className="usage-section">
                <div className="usage-section-header">
                  <h3>Push-to-talk key</h3>
                </div>
                <p className="claude-key-note">
                  Hold this key to dictate into the active session (a mic icon appears in the
                  sidebar while held); release to stop and send whatever was recognized. Click
                  Change and press whichever key you'd like — only a modifier held alone
                  (Option, Shift, Control, or right Command) can actually work as push-to-talk,
                  the only kind of key macOS reports a reliable press/release for regardless of
                  what else is held; anything else is rejected with an explanation.
                </p>
                <div className="claude-key-active-row">
                  <span className={`ptt-key-display ${pttRecording ? "recording" : ""}`}>
                    {pttRecording
                      ? "Press a key… (Esc to cancel)"
                      : (pttOptions.find(([code]) => code === (pttKeycode ?? pttOptions[0][0]))?.[1] ??
                        "Not set")}
                  </span>
                  {!pttRecording && (
                    <button
                      onClick={() => {
                        setPttRecording(true);
                        setPttCaptureError(null);
                      }}
                    >
                      Change
                    </button>
                  )}
                </div>
                {pttCaptureError && <p className="ptt-capture-error">{pttCaptureError}</p>}
                {pttSaved && <span className="settings-saved-hint">Saved</span>}
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
