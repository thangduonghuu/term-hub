import { forwardRef } from "react";
import { TerminalView, type TerminalHandle } from "./TerminalView";
import type { SessionInfo } from "../lib/api";
import type { SessionStatus } from "../lib/status";

interface Props {
  session: SessionInfo;
  active: boolean;
  status: SessionStatus;
  onFocus: (id: string) => void;
  onClose: (id: string) => void;
  onOpenExternal: (cwd: string) => void;
  canOpenExternal: boolean;
}

export const TerminalPane = forwardRef<TerminalHandle, Props>(function TerminalPane(
  { session, active, status, onFocus, onClose, onOpenExternal, canOpenExternal },
  ref,
) {
  return (
    <div
      id={`pane-${session.id}`}
      className={`terminal-pane ${active ? "active" : ""} ${status === "exited" ? "exited" : ""}`}
      onMouseDown={() => onFocus(session.id)}
    >
      <div className="pane-header">
        <span className="pane-dots">
          <span className="dot red" />
          <span className="dot yellow" />
          <span className="dot green" />
        </span>
        <span className="pane-title" title={session.cwd}>
          {session.name}
          {status === "exited" && " (exited)"}
        </span>
        {canOpenExternal && (
          <button
            className="pane-external-btn"
            title="Open this folder in an external terminal"
            onClick={(e) => {
              e.stopPropagation();
              onOpenExternal(session.cwd);
            }}
          >
            ⤢
          </button>
        )}
        <button
          className="pane-close-btn"
          title="Close session"
          onClick={(e) => {
            e.stopPropagation();
            onClose(session.id);
          }}
        >
          ×
        </button>
      </div>
      <div className="pane-body">
        <TerminalView ref={ref} sessionId={session.id} />
      </div>
    </div>
  );
});
