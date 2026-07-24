import { useEffect, useState } from "react";
import { api, type SessionInfo } from "./lib/api";
import { Sidebar } from "./components/Sidebar";
import { UsageDashboard } from "./components/UsageDashboard";
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
  const [recentlyActive, setRecentlyActive] = useState<Set<string>>(new Set());
  const [exitedIds, setExitedIds] = useState<Set<string>>(new Set());

  useEffect(() => {
    api.listSessions().then(setSessions);
  }, []);

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

  async function handleDuplicate(session: SessionInfo) {
    const created = await api.createSession(`${session.name} copy`, session.cwd);
    setSessions((prev) => [...prev, created]);
    setActiveId(created.id);
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

  return (
    <div className="app-shell">
      <Sidebar
        sessions={sessions}
        activeId={activeId}
        recentlyActive={recentlyActive}
        exitedIds={exitedIds}
        onNew={handleNew}
        onClose={handleClose}
        onRename={handleRename}
        onSelect={handleSelect}
        onDuplicate={handleDuplicate}
        onNewInFolder={handleNewInFolder}
        onOpenUsage={() => setShowUsage(true)}
        pendingRenameId={pendingRenameId}
        onPendingRenameHandled={() => setPendingRenameId(null)}
      />
      {showUsage && <UsageDashboard onClose={() => setShowUsage(false)} />}
    </div>
  );
}

export default App;
