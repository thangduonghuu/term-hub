import { useEffect, useRef, useState } from "react";
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

export function MessageLog() {
  const [entries, setEntries] = useState<LogEntry[]>([]);
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    api.getMessageLog().then(setEntries).catch(() => {});

    function onMessage(e: Event) {
      const d = (e as CustomEvent<LiveMessage>).detail;
      setEntries((prev) => [
        ...prev,
        { from_name: d.from, to_name: d.to, body: d.body, created_at: d.ts },
      ]);
    }
    window.addEventListener("termhub:message", onMessage);
    return () => window.removeEventListener("termhub:message", onMessage);
  }, []);

  useEffect(() => {
    endRef.current?.scrollIntoView({ block: "end" });
  }, [entries]);

  return (
    <div className="msglog">
      <div className="msglog-header">Messages</div>
      <div className="msglog-body">
        {entries.length === 0 ? (
          <div className="msglog-empty">
            No messages yet. Sessions message each other with <code>termhub-msg send</code>.
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
  );
}

function formatTime(unixSecs: number): string {
  return new Date(unixSecs * 1000).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
  });
}
