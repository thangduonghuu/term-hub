import { useEffect, useRef, useState } from "react";
import { MessageSquare, X } from "lucide-react";
import { api } from "../lib/api";
import { sessionColor, sessionTint } from "../lib/sessionColor";
import "./MessageLog.css";

// One rendered line. `num` is the endpoint's `#N` when it's an open session (Rust resolves it
// against the session list — see `AppEvent::MessageLogged`); `id` is kept for a since-closed
// session so its old bubbles keep a stable hash colour; both null for an outside shell.
interface Entry {
  fromId: string | null;
  fromNum: number | null;
  fromName: string | null;
  toId: string | null;
  toNum: number | null;
  toName: string | null;
  body: string;
  ts: number;
}

// The `termhub:message` DOM event Rust pushes on every delivered message.
interface LiveMessage {
  fromId: string | null;
  fromNum: number | null;
  from: string | null;
  toId: string | null;
  toNum: number | null;
  to: string | null;
  body: string;
  ts: number;
}

// Mirrors Rust's `LogPanel` (see `lib.rs`), pushed as the `termhub:log-state` detail.
type PanelState = "hidden" | "collapsed" | "open";

export function MessageLog() {
  const [entries, setEntries] = useState<Entry[]>([]);
  // Starts "hidden"; Rust flips it to "collapsed" on the first message and toggles
  // "collapsed" <-> "open" from the rail / the panel's ×.
  const [panel, setPanel] = useState<PanelState>("hidden");
  // Messages seen while the panel wasn't open — shown as a badge on the rail, cleared on open.
  const [unseen, setUnseen] = useState(0);
  const endRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<PanelState>("hidden");

  useEffect(() => {
    api
      .getMessageLog()
      .then((rows) =>
        setEntries(
          rows.map((m) => ({
            fromId: m.from_id,
            fromNum: null,
            fromName: m.from_name,
            toId: m.to_id,
            toNum: null,
            toName: m.to_name,
            body: m.body,
            ts: m.created_at,
          })),
        ),
      )
      .catch(() => {});

    function onMessage(e: Event) {
      const d = (e as CustomEvent<LiveMessage>).detail;
      setEntries((prev) => [
        ...prev,
        {
          fromId: d.fromId,
          fromNum: d.fromNum,
          fromName: d.from,
          toId: d.toId,
          toNum: d.toNum,
          toName: d.to,
          body: d.body,
          ts: d.ts,
        },
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

  function endpoint(
    id: string | null,
    num: number | null,
    name: string | null,
    fallback: string,
  ) {
    if (num && num > 0) {
      // `#N Name`, matching how the sidebar reads (the chip is the `#N`, the name follows).
      return {
        label: name ? `#${num} ${name}` : `#${num}`,
        color: sessionColor(id ?? "", num),
        tint: sessionTint(id ?? "", num),
      };
    }
    // A closed session (id, no live #N) keeps its stable hash colour; an outside shell /
    // broadcast has neither.
    return {
      label: name ?? fallback,
      color: id ? sessionColor(id) : "#8a8a8a",
      tint: id ? sessionTint(id) : "rgba(255,255,255,0.06)",
    };
  }

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
            entries.map((m, i) => {
              const from = endpoint(m.fromId, m.fromNum, m.fromName, "outside");
              const to = endpoint(m.toId, m.toNum, m.toName, "everyone");
              const prev = entries[i - 1];
              // Group consecutive messages on the same #A → #B pair (Messenger-style): only the
              // first in a run carries the header.
              const grouped =
                prev &&
                prev.fromId === m.fromId &&
                prev.toId === m.toId &&
                prev.fromNum === m.fromNum &&
                prev.toNum === m.toNum;
              return (
                <div className={`msglog-row ${grouped ? "grouped" : ""}`} key={i}>
                  {!grouped && (
                    <div className="msglog-meta">
                      <span className="msglog-chip" style={{ color: from.color, background: from.tint }}>
                        {from.label}
                      </span>
                      <span className="msglog-arrow">→</span>
                      <span className="msglog-chip" style={{ color: to.color, background: to.tint }}>
                        {to.label}
                      </span>
                      <span className="msglog-time">{formatTime(m.ts)}</span>
                    </div>
                  )}
                  <div
                    className="msglog-bubble"
                    style={{ borderLeftColor: from.color, background: from.tint }}
                  >
                    {m.body}
                  </div>
                </div>
              );
            })
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
