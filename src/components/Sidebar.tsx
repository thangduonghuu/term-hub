import { useEffect, useMemo, useState } from "react";
import {
  Copy,
  ExternalLink,
  FolderPlus,
  History,
  Mail,
  Mic,
  Pencil,
  Plus,
  Server,
  Settings,
  X,
} from "lucide-react";
import type { SessionInfo } from "../lib/api";
import { folderName } from "../lib/path";
import { sessionColor, sessionTint } from "../lib/sessionColor";
import { LumenPromo } from "./LumenPromo";

// Rough menu box, for clamping it inside the narrow sidebar webview.
const CTX_MENU_W = 208;
const CTX_MENU_H = 190;

interface Props {
  sessions: SessionInfo[];
  activeId: string | null;
  recentlyActive: Set<string>;
  voiceRecording: boolean;
  exitedIds: Set<string>;
  unreadBySession: Record<string, number>;
  onNew: () => void;
  onClose: (id: string) => void;
  onRename: (id: string, name: string) => void;
  onSelect: (id: string) => void;
  onDuplicate: (session: SessionInfo) => void;
  onResumeClaude: (session: SessionInfo) => void;
  onNewInFolder: (cwd: string) => void;
  onOpenExternal: (session: SessionInfo) => void;
  onOpenSsh: () => void;
  onOpenSettings: () => void;
  pendingRenameId: string | null;
  onPendingRenameHandled: () => void;
}

export function Sidebar({
  sessions,
  activeId,
  recentlyActive,
  voiceRecording,
  exitedIds,
  unreadBySession,
  onNew,
  onClose,
  onRename,
  onSelect,
  onDuplicate,
  onResumeClaude,
  onNewInFolder,
  onOpenExternal,
  onOpenSsh,
  onOpenSettings,
  pendingRenameId,
  onPendingRenameHandled,
}: Props) {
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draftName, setDraftName] = useState("");
  const [query, setQuery] = useState("");
  // Only the double-click rename path (`startRename`) should steal keyboard focus into the
  // sidebar webview. The auto-opened field below must not — a newly created session should keep
  // real keyboard focus on its native terminal tile (which Rust already focuses on spawn), not
  // have this input's `autoFocus` yank it back into the sidebar.
  const [autoFocusEdit, setAutoFocusEdit] = useState(false);
  // Right-click menu for a session row — replaces the old always-there hover buttons.
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number; session: SessionInfo } | null>(
    null,
  );

  // New sessions open straight into an editable, blank name field instead of a
  // generic default label — the user names it right away instead of double-clicking later.
  useEffect(() => {
    if (pendingRenameId && sessions.some((s) => s.id === pendingRenameId)) {
      setEditingId(pendingRenameId);
      setDraftName("");
      setAutoFocusEdit(false);
      onPendingRenameHandled();
    }
  }, [pendingRenameId, sessions, onPendingRenameHandled]);

  function startRename(session: SessionInfo) {
    setEditingId(session.id);
    setDraftName(session.name);
    setAutoFocusEdit(true);
  }

  function commitRename(id: string) {
    const trimmed = draftName.trim();
    if (trimmed) onRename(id, trimmed);
    setEditingId(null);
  }

  function openContextMenu(e: React.MouseEvent, session: SessionInfo) {
    e.preventDefault();
    e.stopPropagation();
    const x = Math.min(e.clientX, window.innerWidth - CTX_MENU_W - 4);
    const y = Math.min(e.clientY, window.innerHeight - CTX_MENU_H - 4);
    setCtxMenu({ x: Math.max(4, x), y: Math.max(4, y), session });
  }

  // A click elsewhere, Escape, or scrolling the list dismisses the context menu. Deliberately
  // NOT window `blur` — in this multi-surface app focus hops between the webview and the native
  // terminal views constantly, which would close the menu the instant it opened.
  useEffect(() => {
    if (!ctxMenu) return;
    const close = () => setCtxMenu(null);
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("click", close);
    window.addEventListener("keydown", onKey);
    document
      .querySelector(".session-groups")
      ?.addEventListener("scroll", close, { passive: true });
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("keydown", onKey);
      document.querySelector(".session-groups")?.removeEventListener("scroll", close);
    };
  }, [ctxMenu]);

  // `#N` — 1-based position in creation order, matching what `control.rs` assigns so
  // `termhub-msg send #2 …` hits the same session shown here. Renumbers if an earlier session
  // closes.
  const sessionNum = useMemo(() => {
    const m = new Map<string, number>();
    sessions.forEach((s, i) => m.set(s.id, i + 1));
    return m;
  }, [sessions]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return sessions;
    const asNum = q.replace(/^#/, "");
    return sessions.filter(
      (s) =>
        s.name.toLowerCase().includes(q) ||
        s.cwd.toLowerCase().includes(q) ||
        String(sessionNum.get(s.id)) === asNum,
    );
  }, [sessions, query, sessionNum]);

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
          {voiceRecording && (
            <span
              className="mic-recording-indicator"
              title="Dictating… (release the push-to-talk key, set in Settings, to stop)"
            >
              <Mic size={15} />
            </span>
          )}
          <button className="usage-toggle-btn" onClick={onOpenSettings} title="Settings">
            <Settings size={15} />
          </button>
          <button className="new-session-btn" onClick={onOpenSsh} title="Connect to VPS (SSH)">
            <Server size={15} />
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
                    onContextMenu={(e) => openContextMenu(e, session)}
                  >
                    <span
                      className="session-num"
                      style={{
                        color: sessionColor(session.id, sessionNum.get(session.id)),
                        backgroundColor: sessionTint(session.id, sessionNum.get(session.id)),
                      }}
                      title={`Session #${sessionNum.get(session.id)} — target it with \`termhub-msg send #${sessionNum.get(session.id)} …\``}
                    >
                      #{sessionNum.get(session.id)}
                    </span>
                    {editingId === session.id ? (
                      <input
                        autoFocus={autoFocusEdit}
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
                    {(unreadBySession[session.id] ?? 0) > 0 && (
                      <span
                        className="unread-badge"
                        title={`${unreadBySession[session.id]} unread message${
                          unreadBySession[session.id] === 1 ? "" : "s"
                        } — run \`termhub-msg inbox\` in this session`}
                      >
                        <Mail size={11} />
                        {unreadBySession[session.id]}
                      </span>
                    )}
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

      {ctxMenu && (
        <ul
          className="session-ctx-menu"
          style={{ left: ctxMenu.x, top: ctxMenu.y }}
          onClick={(e) => e.stopPropagation()}
        >
          <li onClick={() => { startRename(ctxMenu.session); setCtxMenu(null); }}>
            <Pencil size={13} /> Rename
          </li>
          <li onClick={() => { onResumeClaude(ctxMenu.session); setCtxMenu(null); }}>
            <History size={13} /> Resume Claude Code
          </li>
          <li onClick={() => { onOpenExternal(ctxMenu.session); setCtxMenu(null); }}>
            <ExternalLink size={13} /> Open in external terminal
          </li>
          <li onClick={() => { onDuplicate(ctxMenu.session); setCtxMenu(null); }}>
            <Copy size={13} /> Duplicate session
          </li>
          <li
            className="session-ctx-danger"
            onClick={() => { onClose(ctxMenu.session.id); setCtxMenu(null); }}
          >
            <X size={13} /> Close session
          </li>
        </ul>
      )}
    </aside>
  );
}
