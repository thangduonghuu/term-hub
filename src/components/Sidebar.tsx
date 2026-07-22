import { useEffect, useMemo, useState } from "react";
import type { SessionInfo } from "../lib/api";
import type { SessionStatus } from "../lib/status";
import { folderName } from "../lib/path";

interface Props {
  sessions: SessionInfo[];
  statuses: Record<string, SessionStatus>;
  activeId: string | null;
  onSelect: (id: string) => void;
  onNew: () => void;
  onClose: (id: string) => void;
  onRename: (id: string, name: string) => void;
  onReopen: (id: string) => void;
  onDuplicate: (session: SessionInfo) => void;
  onNewInFolder: (cwd: string) => void;
  terminalApps: string[];
  externalApp: string;
  onExternalAppChange: (app: string) => void;
  pendingRenameId: string | null;
  onPendingRenameHandled: () => void;
}

const STATUS_LABEL: Record<SessionStatus, string> = {
  running: "running",
  idle: "idle (no output recently)",
  exited: "exited — click to reopen",
  closed: "closed — click to reopen",
};

export function Sidebar({
  sessions,
  statuses,
  activeId,
  onSelect,
  onNew,
  onClose,
  onRename,
  onReopen,
  onDuplicate,
  onNewInFolder,
  terminalApps,
  externalApp,
  onExternalAppChange,
  pendingRenameId,
  onPendingRenameHandled,
}: Props) {
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draftName, setDraftName] = useState("");
  const [query, setQuery] = useState("");

  // New sessions open straight into an editable, blank name field instead of a
  // generic default label — the user names it right away instead of double-clicking later.
  useEffect(() => {
    if (pendingRenameId && sessions.some((s) => s.id === pendingRenameId)) {
      setEditingId(pendingRenameId);
      setDraftName("");
      onPendingRenameHandled();
    }
  }, [pendingRenameId, sessions, onPendingRenameHandled]);

  function startRename(session: SessionInfo) {
    setEditingId(session.id);
    setDraftName(session.name);
  }

  function commitRename(id: string) {
    const trimmed = draftName.trim();
    if (trimmed) onRename(id, trimmed);
    setEditingId(null);
  }

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return sessions;
    return sessions.filter(
      (s) => s.name.toLowerCase().includes(q) || s.cwd.toLowerCase().includes(q),
    );
  }, [sessions, query]);

  const groups = useMemo(() => {
    const map = new Map<string, SessionInfo[]>();
    for (const s of filtered) {
      const list = map.get(s.cwd) ?? [];
      list.push(s);
      map.set(s.cwd, list);
    }
    return Array.from(map.entries()).sort((a, b) => a[0].localeCompare(b[0]));
  }, [filtered]);

  return (
    <aside className="sidebar">
      <div className="sidebar-header">
        <span>Sessions</span>
        <button className="new-session-btn" onClick={onNew} title="New session">
          +
        </button>
      </div>
      <div className="sidebar-search">
        <input
          type="text"
          placeholder="Filter by name or path…"
          value={query}
          onChange={(e) => setQuery(e.currentTarget.value)}
        />
      </div>
      <div className="session-groups">
        {groups.map(([cwd, group]) => (
          <div key={cwd} className="session-group">
            <div className="group-header">
              <span className="group-name" title={cwd}>
                {folderName(cwd)}
              </span>
              <button
                className="group-new-btn"
                title="New session in this folder"
                onClick={() => onNewInFolder(cwd)}
              >
                +
              </button>
            </div>
            <ul className="session-list">
              {group.map((session) => {
                const status = statuses[session.id] ?? "closed";
                return (
                  <li
                    key={session.id}
                    className={`session-item ${session.id === activeId ? "active" : ""}`}
                    onClick={() =>
                      status === "running" || status === "idle"
                        ? onSelect(session.id)
                        : onReopen(session.id)
                    }
                  >
                    <span className={`status-dot ${status}`} title={STATUS_LABEL[status]} />
                    {editingId === session.id ? (
                      <input
                        autoFocus
                        className="rename-input"
                        placeholder={session.name}
                        value={draftName}
                        onChange={(e) => setDraftName(e.currentTarget.value)}
                        onBlur={() => commitRename(session.id)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") commitRename(session.id);
                          if (e.key === "Escape") setEditingId(null);
                        }}
                        onClick={(e) => e.stopPropagation()}
                      />
                    ) : (
                      <span
                        className="session-name"
                        onDoubleClick={(e) => {
                          e.stopPropagation();
                          startRename(session);
                        }}
                        title={session.cwd}
                      >
                        {session.name}
                      </span>
                    )}
                    <button
                      className="duplicate-btn"
                      title="Duplicate session"
                      onClick={(e) => {
                        e.stopPropagation();
                        onDuplicate(session);
                      }}
                    >
                      ⧉
                    </button>
                    <button
                      className="close-btn"
                      title="Close session"
                      onClick={(e) => {
                        e.stopPropagation();
                        onClose(session.id);
                      }}
                    >
                      ×
                    </button>
                  </li>
                );
              })}
            </ul>
          </div>
        ))}
        {groups.length === 0 && <div className="no-results">No matching sessions.</div>}
      </div>
      {terminalApps.length > 0 && (
        <div className="sidebar-settings">
          <label htmlFor="external-app-select">Open externally with</label>
          <select
            id="external-app-select"
            value={externalApp}
            onChange={(e) => onExternalAppChange(e.currentTarget.value)}
          >
            {terminalApps.map((app) => (
              <option key={app} value={app}>
                {app}
              </option>
            ))}
          </select>
        </div>
      )}
    </aside>
  );
}
