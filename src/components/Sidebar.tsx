import { useEffect, useMemo, useState } from "react";
import { BarChart3, Copy, ExternalLink, FolderOpen, FolderPlus, Plus, Settings, X } from "lucide-react";
import type { SessionInfo } from "../lib/api";
import { folderName } from "../lib/path";
import { LumenPromo } from "./LumenPromo";

interface Props {
  sessions: SessionInfo[];
  activeId: string | null;
  recentlyActive: Set<string>;
  exitedIds: Set<string>;
  onNew: () => void;
  onClose: (id: string) => void;
  onRename: (id: string, name: string) => void;
  onSelect: (id: string) => void;
  onDuplicate: (session: SessionInfo) => void;
  onNewInFolder: (cwd: string) => void;
  onOpenFolder: () => void;
  onOpenExternal: (session: SessionInfo) => void;
  onOpenUsage: () => void;
  onOpenSettings: () => void;
  pendingRenameId: string | null;
  onPendingRenameHandled: () => void;
}

export function Sidebar({
  sessions,
  activeId,
  recentlyActive,
  exitedIds,
  onNew,
  onClose,
  onRename,
  onSelect,
  onDuplicate,
  onNewInFolder,
  onOpenFolder,
  onOpenExternal,
  onOpenUsage,
  onOpenSettings,
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
        <div className="sidebar-header-actions">
          <button className="usage-toggle-btn" onClick={onOpenUsage} title="Token usage">
            <BarChart3 size={15} />
          </button>
          <button className="usage-toggle-btn" onClick={onOpenSettings} title="Settings">
            <Settings size={15} />
          </button>
          <button className="new-session-btn" onClick={onOpenFolder} title="Open folder… (Ctrl+R)">
            <FolderOpen size={15} />
          </button>
          <button className="new-session-btn" onClick={onNew} title="New session">
            <Plus size={16} />
          </button>
        </div>
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
                <FolderPlus size={13} />
              </button>
            </div>
            <ul className="session-list">
              {group.map((session) => {
                return (
                  <li
                    key={session.id}
                    className={`session-item ${session.id === activeId ? "active" : ""}`}
                    onClick={() => onSelect(session.id)}
                  >
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
                        {exitedIds.has(session.id) ? (
                          <span
                            className="exited-dot"
                            title="Process exited — click the session to restart it"
                          />
                        ) : (
                          recentlyActive.has(session.id) && (
                            <span className="activity-dot" title="Recent output" />
                          )
                        )}
                        {session.name}
                      </span>
                    )}
                    <button
                      className="duplicate-btn"
                      title="Open this folder in an external terminal"
                      onClick={(e) => {
                        e.stopPropagation();
                        onOpenExternal(session);
                      }}
                    >
                      <ExternalLink size={13} />
                    </button>
                    <button
                      className="duplicate-btn"
                      title="Duplicate session"
                      onClick={(e) => {
                        e.stopPropagation();
                        onDuplicate(session);
                      }}
                    >
                      <Copy size={13} />
                    </button>
                    <button
                      className="close-btn"
                      title="Close session"
                      onClick={(e) => {
                        e.stopPropagation();
                        onClose(session.id);
                      }}
                    >
                      <X size={14} />
                    </button>
                  </li>
                );
              })}
            </ul>
          </div>
        ))}
        {groups.length === 0 && query.trim() && (
          <div className="no-results">No matching sessions.</div>
        )}
      </div>
      <LumenPromo />
    </aside>
  );
}
