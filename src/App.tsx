import { useEffect, useState } from "react";
import { api, type SessionInfo } from "./lib/api";
import { Sidebar } from "./components/Sidebar";
import { UsageDashboard } from "./components/UsageDashboard";
import { SettingsPanel } from "./components/SettingsPanel";
import { QuickOpen } from "./components/QuickOpen";
import "./App.css";

// A session counts as "recently active" (shows the sidebar's activity dot) if it produced
// pty output within this many ms — long enough to stay lit through a burst of fast output,
// short enough to turn off soon after a command actually finishes.
const ACTIVITY_WINDOW_MS = 3000;
const ACTIVITY_POLL_MS = 1000;
// Same cadence as activity — exited status changes about as rarely as a shell process dies,
// but there's no push channel for it (see `get_exited_sessions`'s doc comment), so poll it.
const EXITED_POLL_MS = 1000;

function App() {
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [pendingRenameId, setPendingRenameId] = useState<string | null>(null);
  const [showUsage, setShowUsage] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [showQuickOpen, setShowQuickOpen] = useState(false);
  const [recentlyActive, setRecentlyActive] = useState<Set<string>>(new Set());
  const [exitedIds, setExitedIds] = useState<Set<string>>(new Set());
  const [voiceError, setVoiceError] = useState<string | null>(null);
  const [voiceRecording, setVoiceRecording] = useState(false);

  useEffect(() => {
    api.listSessions().then(setSessions);
    // Seeds the sidebar's highlight with whichever tile `App::resumed` picked as active on the
    // Rust side at startup — `handleSelect` etc. keep this in sync from here on, but nothing
    // else tells this app what Rust started with.
    api.getActiveSession().then(setActiveId);
    // Seeds `--accent-color` (App.css's fallback covers the initial paint before this resolves)
    // from whatever's saved in the db — `SettingsPanel` sets this same property live when the
    // user picks a different color in Settings > Appearance.
    api.getAccentColor().then((color) => {
      if (color) document.documentElement.style.setProperty("--accent-color", color);
    });
  }, []);

  // Both the usage dashboard and settings are centered-overlay modals rendered inside the
  // sidebar webview, which is normally kept narrow (just the sidebar strip) so clicks past it
  // reach the native terminal tiles instead of being captured by the webview. Their CSS only
  // has as much viewport to center itself in as the webview actually is, so widen the webview
  // to the full window while either is open, and narrow it back once both are closed.
  useEffect(() => {
    api.setOverlayOpen(showUsage || showSettings || showQuickOpen);
  }, [showUsage, showSettings, showQuickOpen]);

  useEffect(() => {
    const poll = () => {
      api.getActivity().then((activity) => {
        const now = Date.now();
        const next = new Set<string>();
        for (const [id, lastMs] of Object.entries(activity)) {
          if (now - lastMs < ACTIVITY_WINDOW_MS) next.add(id);
        }
        setRecentlyActive(next);
      });
    };
    poll();
    const interval = setInterval(poll, ACTIVITY_POLL_MS);
    return () => clearInterval(interval);
  }, []);

  useEffect(() => {
    const poll = () => {
      api.getExitedSessions().then((ids) => setExitedIds(new Set(ids)));
    };
    poll();
    const interval = setInterval(poll, EXITED_POLL_MS);
    return () => clearInterval(interval);
  }, []);

  async function handleNew() {
    const created = await api.createSession();
    setSessions((prev) => [...prev, created]);
    setPendingRenameId(created.id);
    setActiveId(created.id);
  }

  async function handleNewInFolder(cwd: string) {
    const created = await api.createSession(undefined, cwd);
    setSessions((prev) => [...prev, created]);
    setPendingRenameId(created.id);
    setActiveId(created.id);
  }

  // VSCode's Cmd+R "Open Recent" — opens the quick-pick over previously-opened folders instead
  // of going straight to a native browse dialog (that's the picker's own "Browse…" row).
  function handleOpenFolder() {
    setShowQuickOpen(true);
  }

  function handleQuickOpenSelect(path: string) {
    setShowQuickOpen(false);
    handleNewInFolder(path);
  }

  async function handleDuplicate(session: SessionInfo) {
    const created = await api.createSession(`${session.name} copy`, session.cwd);
    setSessions((prev) => [...prev, created]);
    setActiveId(created.id);
  }

  // Sidebar's "Resume Claude" button — `--continue` (not `--resume`, which needs an interactive
  // picker) since it needs no session id: it just resumes the most recent conversation for the
  // session's own cwd, which is exactly what's already unique about each TermHub session. Also
  // focuses the session so the resumed conversation is what the user's looking at.
  function handleResumeClaude(session: SessionInfo) {
    handleSelect(session.id);
    api.sendToSession(session.id, "claude --continue\r");
  }

  // Phase 3: every session has a live tiled terminal on the Rust side (see lib.rs's
  // `App.terms`) — clicking it in the sidebar both highlights it here and hands it real
  // keyboard focus over there.
  function handleSelect(id: string) {
    setActiveId(id);
    api.focusSession(id);
  }

  async function handleClose(id: string) {
    await api.closeSession(id);
    setSessions((prev) => prev.filter((s) => s.id !== id));
    if (activeId === id) setActiveId(null);
  }

  async function handleRename(id: string, name: string) {
    await api.renameSession(id, name);
    setSessions((prev) => prev.map((s) => (s.id === id ? { ...s, name } : s)));
  }

  // Pops a session's folder open in a separate terminal app, alongside (not instead of) the
  // built-in one. Uses the saved preference from Settings if there is one; otherwise falls back
  // to whatever's auto-detected as installed, so this works with zero configuration too.
  async function handleOpenExternal(session: SessionInfo) {
    const preferred = await api.getPreferredTerminalApp();
    const app = preferred || (await api.listTerminalApps())[0];
    if (!app) {
      setShowSettings(true);
      return;
    }
    api.openExternalTerminal(app, session.cwd);
  }

  // New/close/next/prev-session keyboard shortcuts (Cmd+T/Cmd+W/Cmd+Shift+]/Cmd+Shift+[) are
  // caught natively by `macos_input_view.rs` (this webview never has keyboard focus, so a
  // regular `keydown` listener here would never fire) and forwarded in as this DOM event —
  // see `AppEvent::KeyboardShortcut`'s doc comment. Handled here rather than natively since
  // `sessions`/`activeId` are this component's state, not the Rust side's.
  useEffect(() => {
    function onShortcut(e: Event) {
      const action = (e as CustomEvent<string>).detail;
      if (action === "new-session") {
        handleNew();
      } else if (action === "open-folder") {
        handleOpenFolder();
      } else if (action === "close-session") {
        if (activeId) handleClose(activeId);
      } else if (action === "next-session" || action === "prev-session") {
        if (sessions.length === 0) return;
        const idx = sessions.findIndex((s) => s.id === activeId);
        const delta = action === "next-session" ? 1 : -1;
        const nextIdx = idx === -1 ? 0 : (idx + delta + sessions.length) % sessions.length;
        handleSelect(sessions[nextIdx].id);
      }
    }
    window.addEventListener("termhub:shortcut", onShortcut);
    return () => window.removeEventListener("termhub:shortcut", onShortcut);
  }, [sessions, activeId]);

  // Clicking a terminal tile directly (rather than selecting it from the sidebar) changes
  // `active_id` on the Rust side without going through `handleSelect` — see the `MouseInput`
  // handler in lib.rs, which dispatches this event so the sidebar's highlight follows. Just
  // mirrors the state here; no need to call `api.focusSession` back, the tile already has focus.
  useEffect(() => {
    function onSessionFocused(e: Event) {
      setActiveId((e as CustomEvent<string>).detail);
    }
    window.addEventListener("termhub:session-focused", onSessionFocused);
    return () => window.removeEventListener("termhub:session-focused", onSessionFocused);
  }, []);

  // Dismiss any open overlay when the window loses focus (Cmd+Tab away, clicking another app,
  // etc.) or Escape is pressed — see `dismiss_overlays` in lib.rs, which fires this for both.
  // Left open, a modal would be stranded on screen behind whatever the user switched to.
  useEffect(() => {
    function onDismiss() {
      setShowUsage(false);
      setShowSettings(false);
      setShowQuickOpen(false);
    }
    window.addEventListener("termhub:close-overlays", onDismiss);
    return () => window.removeEventListener("termhub:close-overlays", onDismiss);
  }, []);

  // Cmd+Shift+V dictation (see `speech.rs`) failing — denied mic/speech permission, no
  // recognizer available, etc. — is detected natively in Rust, which has no UI of its own to
  // show it in, so it's forwarded here the same way `termhub:shortcut` is. Auto-dismisses so a
  // stale permission error doesn't linger forever over the sidebar.
  useEffect(() => {
    function onVoiceError(e: Event) {
      setVoiceError((e as CustomEvent<string>).detail);
    }
    window.addEventListener("termhub:voice-error", onVoiceError);
    return () => window.removeEventListener("termhub:voice-error", onVoiceError);
  }, []);

  useEffect(() => {
    if (!voiceError) return;
    const timer = setTimeout(() => setVoiceError(null), 6000);
    return () => clearTimeout(timer);
  }, [voiceError]);

  // Push-to-talk (Cmd+Shift+V, held) — pushed rather than polled so the sidebar's mic icon
  // lights up/off the instant the key is pressed/released, not on the next poll tick.
  useEffect(() => {
    function onVoiceState(e: Event) {
      setVoiceRecording((e as CustomEvent<boolean>).detail);
    }
    window.addEventListener("termhub:voice-state", onVoiceState);
    return () => window.removeEventListener("termhub:voice-state", onVoiceState);
  }, []);

  return (
    <div className="app-shell">
      {voiceError && (
        <div className="voice-error-banner" onClick={() => setVoiceError(null)}>
          {voiceError}
        </div>
      )}
      <Sidebar
        sessions={sessions}
        activeId={activeId}
        recentlyActive={recentlyActive}
        voiceRecording={voiceRecording}
        exitedIds={exitedIds}
        onNew={handleNew}
        onClose={handleClose}
        onRename={handleRename}
        onSelect={handleSelect}
        onDuplicate={handleDuplicate}
        onResumeClaude={handleResumeClaude}
        onNewInFolder={handleNewInFolder}
        onOpenFolder={handleOpenFolder}
        onOpenExternal={handleOpenExternal}
        onOpenUsage={() => setShowUsage(true)}
        onOpenSettings={() => setShowSettings(true)}
        pendingRenameId={pendingRenameId}
        onPendingRenameHandled={() => setPendingRenameId(null)}
      />
      {showUsage && <UsageDashboard onClose={() => setShowUsage(false)} />}
      {showSettings && <SettingsPanel onClose={() => setShowSettings(false)} />}
      {showQuickOpen && (
        <QuickOpen onSelect={handleQuickOpenSelect} onClose={() => setShowQuickOpen(false)} />
      )}
    </div>
  );
}

export default App;
