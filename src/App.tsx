import { useEffect, useState } from "react";
import { api, type SessionInfo } from "./lib/api";
import { Sidebar } from "./components/Sidebar";
import { UsageDashboard } from "./components/UsageDashboard";
import "./App.css";

function App() {
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [pendingRenameId, setPendingRenameId] = useState<string | null>(null);
  const [showUsage, setShowUsage] = useState(false);

  useEffect(() => {
    api.listSessions().then(setSessions);
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
