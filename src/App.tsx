import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, type SessionInfo } from "./lib/api";
import { Sidebar } from "./components/Sidebar";
import { TerminalPane } from "./components/TerminalPane";
import type { TerminalHandle } from "./components/TerminalView";
import { computeStatus, type SessionStatus } from "./lib/status";
import "./App.css";

// Lays panes out in as square a grid as possible (2 -> 2x1, 3/4 -> 2x2, 5/6 -> 3x2, ...)
// so N terminals always divide the screen evenly, like a tiling window manager.
function gridColumns(count: number): number {
  if (count <= 1) return 1;
  return Math.ceil(Math.sqrt(count));
}

const EXTERNAL_APP_STORAGE_KEY = "termhub.externalApp";
const IDLE_TICK_MS = 5_000;

function App() {
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [terminalApps, setTerminalApps] = useState<string[]>([]);
  const [externalApp, setExternalApp] = useState<string>(
    () => localStorage.getItem(EXTERNAL_APP_STORAGE_KEY) ?? "",
  );
  const [pendingRenameId, setPendingRenameId] = useState<string | null>(null);
  // Panes that should render in the grid this app run — separate from `running` so an
  // exited shell's pane stays put (with its scrollback) instead of disappearing.
  const [openPaneIds, setOpenPaneIds] = useState<Set<string>>(new Set());
  const [tick, setTick] = useState(0);
  const paneRefs = useRef<Record<string, TerminalHandle | null>>({});
  const lastActivityRef = useRef<Record<string, number>>({});

  useEffect(() => {
    (async () => {
      const list = await api.listSessions();
      if (list.length === 0) {
        const created = await api.createSession();
        setSessions([created]);
        setOpenPaneIds(new Set([created.id]));
        setActiveId(created.id);
      } else {
        setSessions(list);
        setActiveId(list[0].id);
      }
    })();
  }, []);

  useEffect(() => {
    api.listTerminalApps().then((apps) => {
      setTerminalApps(apps);
      setExternalApp((prev) => (prev && apps.includes(prev) ? prev : (apps[0] ?? "")));
    });
  }, []);

  useEffect(() => {
    let unlistenExit: (() => void) | undefined;
    let unlistenOutput: (() => void) | undefined;

    listen<{ id: string }>("pty-exit", (event) => {
      setSessions((prev) =>
        prev.map((s) => (s.id === event.payload.id ? { ...s, running: false } : s)),
      );
    }).then((fn) => {
      unlistenExit = fn;
    });

    listen<{ id: string; data: string }>("pty-output", (event) => {
      lastActivityRef.current[event.payload.id] = Date.now();
    }).then((fn) => {
      unlistenOutput = fn;
    });

    const interval = setInterval(() => setTick((t) => t + 1), IDLE_TICK_MS);

    return () => {
      unlistenExit?.();
      unlistenOutput?.();
      clearInterval(interval);
    };
  }, []);

  function handleExternalAppChange(app: string) {
    setExternalApp(app);
    localStorage.setItem(EXTERNAL_APP_STORAGE_KEY, app);
  }

  function handleOpenExternal(cwd: string) {
    if (!externalApp) return;
    api.openExternalTerminal(externalApp, cwd).catch((err) => {
      console.error("Failed to open external terminal:", err);
    });
  }

  async function handleNew() {
    const created = await api.createSession();
    setSessions((prev) => [...prev, created]);
    setOpenPaneIds((prev) => new Set(prev).add(created.id));
    setActiveId(created.id);
    setPendingRenameId(created.id);
  }

  async function handleNewInFolder(cwd: string) {
    const created = await api.createSession(undefined, cwd);
    setSessions((prev) => [...prev, created]);
    setOpenPaneIds((prev) => new Set(prev).add(created.id));
    setActiveId(created.id);
    setPendingRenameId(created.id);
  }

  async function handleDuplicate(session: SessionInfo) {
    const created = await api.createSession(`${session.name} copy`, session.cwd);
    setSessions((prev) => [...prev, created]);
    setOpenPaneIds((prev) => new Set(prev).add(created.id));
    setActiveId(created.id);
  }

  function focusPane(id: string) {
    setActiveId(id);
    document.getElementById(`pane-${id}`)?.scrollIntoView({
      behavior: "smooth",
      block: "nearest",
      inline: "nearest",
    });
    paneRefs.current[id]?.focus();
  }

  async function handleReopen(id: string) {
    const info = await api.reopenSession(id);
    lastActivityRef.current[id] = Date.now();
    setSessions((prev) => prev.map((s) => (s.id === id ? info : s)));
    setOpenPaneIds((prev) => new Set(prev).add(id));
    setActiveId(id);
  }

  async function handleClose(id: string) {
    await api.closeSession(id);
    delete paneRefs.current[id];
    delete lastActivityRef.current[id];
    setOpenPaneIds((prev) => {
      const next = new Set(prev);
      next.delete(id);
      return next;
    });
    setSessions((prev) => {
      const next = prev.filter((s) => s.id !== id);
      if (activeId === id) {
        setActiveId(next.length > 0 ? next[0].id : null);
      }
      return next;
    });
  }

  async function handleRename(id: string, name: string) {
    await api.renameSession(id, name);
    setSessions((prev) => prev.map((s) => (s.id === id ? { ...s, name } : s)));
  }

  const openSessions = sessions.filter((s) => openPaneIds.has(s.id));
  const cols = gridColumns(openSessions.length);
  const rows = Math.max(1, Math.ceil(openSessions.length / cols));

  const now = Date.now();
  void tick; // recompute statuses every IDLE_TICK_MS
  const statuses: Record<string, SessionStatus> = {};
  for (const s of sessions) {
    statuses[s.id] = computeStatus(
      s.running,
      openPaneIds.has(s.id),
      lastActivityRef.current[s.id],
      now,
    );
  }

  return (
    <div className="app-shell">
      <Sidebar
        sessions={sessions}
        statuses={statuses}
        activeId={activeId}
        onSelect={focusPane}
        onNew={handleNew}
        onClose={handleClose}
        onRename={handleRename}
        onReopen={handleReopen}
        onDuplicate={handleDuplicate}
        onNewInFolder={handleNewInFolder}
        terminalApps={terminalApps}
        externalApp={externalApp}
        onExternalAppChange={handleExternalAppChange}
        pendingRenameId={pendingRenameId}
        onPendingRenameHandled={() => setPendingRenameId(null)}
      />
      <main
        className="terminal-grid"
        style={{
          gridTemplateColumns: `repeat(${cols}, 1fr)`,
          gridTemplateRows: `repeat(${rows}, 1fr)`,
        }}
      >
        {openSessions.map((s) => (
          <TerminalPane
            key={s.id}
            ref={(handle) => {
              paneRefs.current[s.id] = handle;
            }}
            session={s}
            active={s.id === activeId}
            status={statuses[s.id] ?? "closed"}
            onFocus={focusPane}
            onClose={handleClose}
            onOpenExternal={handleOpenExternal}
            canOpenExternal={terminalApps.length > 0}
          />
        ))}
        {openSessions.length === 0 && (
          <div className="empty-state">No sessions. Click + to start one.</div>
        )}
      </main>
    </div>
  );
}

export default App;
