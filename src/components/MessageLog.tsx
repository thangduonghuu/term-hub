import { useEffect, useRef, useState } from "react";
import { MessageSquare, X } from "lucide-react";
import { api, type LogEntry } from "../lib/api";
import "./MessageLog.css";

// Shape of the `termhub:message` DOM event Rust pushes on every delivered message (see
// `AppEvent::MessageLogged`) — keyed differently from `LogEntry` (`from`/`to`/`ts`).
interface LiveMessage {
  from: string | null;
  to: string | null;
  body: string;
  ts: number;
}

// Mirrors Rust's `LogPanel` (see `lib.rs`), pushed as the `termhub:log-state` detail.
type PanelState = "hidden" | "collapsed" | "open";

export function MessageLog() {
  const [entries, setEntries] = useState<LogEntry[]>([]);
  // Starts "hidden"; Rust flips it to "collapsed" on the first message and toggles
  // "collapsed" <-> "open" from the rail / the panel's ×.
  const [panel, setPanel] = useState<PanelState>("hidden");
  // Messages seen while the panel wasn't open — shown as a badge on the rail, cleared on open.
  const [unseen, setUnseen] = useState(0);
  const endRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<PanelState>("hidden");

  useEffect(() => {
    api.getMessageLog().then(setEntries).catch(() => {});

    function onMessage(e: Event) {
      const d = (e as CustomEvent<LiveMessage>).detail;
      setEntries((prev) => [
        ...prev,
        { from_name: d.from, to_name: d.to, body: d.body, created_at: d.ts },
      ]);
      if (panelRef.current !== "open") setUnseen((n) => n + 1);
    }
    function onState(e: Event) {
      const s = (e as CustomEvent<PanelState>).detail;
      panelRef.current = s;
      setPanel(s);
      if (s === "open") setUnseen(0);
    }
    window.addEventListener("termhub:message", onMessage);
    window.addEventListener("termhub:log-state", onState);
    return () => {
      window.removeEventListener("termhub:message", onMessage);
      window.removeEventListener("termhub:log-state", onState);
    };
  }, []);

  useEffect(() => {
    if (panel === "open") endRef.current?.scrollIntoView({ block: "end" });
  }, [entries, panel]);

  return (
    <div className={`msglog ${panel}`}>
      <button
        className="msglog-rail"
        onClick={() => api.toggleMessageLog()}
        title={panel === "open" ? "Collapse messages" : "Open messages"}
      >
        <MessageSquare size={16} />
        {unseen > 0 && <span className="msglog-rail-badge">{unseen}</span>}
      </button>
      <div className="msglog-panel">
        <div className="msglog-header">
          <span>Messages</span>
          <button
            className="msglog-close"
            onClick={() => api.toggleMessageLog()}
            title="Collapse"
          >
            <X size={14} />
          </button>
        </div>
        <div className="msglog-body">
          {entries.length === 0 ? (
            <div className="msglog-empty">
              No messages yet. Sessions message each other with <code>termhub-msg send</code>{" "}
              (target a session by <code>#num</code>, name, or id).
            </div>
          ) : (
            entries.map((m, i) => (
              <div className="msglog-item" key={i}>
                <div className="msglog-meta">
                  <span className="msglog-from">{m.from_name ?? "outside"}</span>
                  <span className="msglog-arrow">→</span>
                  <span className="msglog-to">{m.to_name ?? "(closed)"}</span>
                  <span className="msglog-time">{formatTime(m.created_at)}</span>
                </div>
                <div className="msglog-text">{m.body}</div>
              </div>
            ))
          )}
          <div ref={endRef} />
        </div>
      </div>
    </div>
  );
}

function formatTime(unixSecs: number): string {
  return new Date(unixSecs * 1000).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
  });
}
