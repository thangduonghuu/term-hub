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
  }

  async function handleNewInFolder(cwd: string) {
    const created = await api.createSession(undefined, cwd);
    setSessions((prev) => [...prev, created]);
    setPendingRenameId(created.id);
  }

  async function handleDuplicate(session: SessionInfo) {
    const created = await api.createSession(`${session.name} copy`, session.cwd);
    setSessions((prev) => [...prev, created]);
  }

  // Phase 1b has a single embedded terminal (not yet one per session — that's Phase 3's
  // multi-session tiling), so selecting a session is just a visual highlight for now.
  function handleSelect(id: string) {
    setActiveId(id);
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
