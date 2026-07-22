import { forwardRef, useEffect, useImperativeHandle, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import "@xterm/xterm/css/xterm.css";
import { api } from "../lib/api";

interface Props {
  sessionId: string;
}

export interface TerminalHandle {
  focus: () => void;
}

export const TerminalView = forwardRef<TerminalHandle, Props>(function TerminalView(
  { sessionId },
  ref,
) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);

  useImperativeHandle(ref, () => ({
    focus: () => termRef.current?.focus(),
  }));

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const term = new Terminal({
      convertEol: true,
      cursorBlink: true,
      fontFamily: "Menlo, Consolas, monospace",
      fontSize: 12,
      theme: { background: "#141414", foreground: "#e6e6e6" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container);
    termRef.current = term;

    fit.fit();
    api.resizePty(sessionId, term.rows, term.cols).catch(() => {});

    const dataDisposable = term.onData((data) => {
      api.writePty(sessionId, data).catch(() => {});
    });

    let unlistenOutput: UnlistenFn | undefined;
    let unlistenExit: UnlistenFn | undefined;

    listen<{ id: string; data: string }>("pty-output", (event) => {
      if (event.payload.id === sessionId) {
        term.write(event.payload.data);
      }
    }).then((fn) => {
      unlistenOutput = fn;
    });

    listen<{ id: string }>("pty-exit", (event) => {
      if (event.payload.id === sessionId) {
        term.write("\r\n\x1b[90m[process exited]\x1b[0m\r\n");
      }
    }).then((fn) => {
      unlistenExit = fn;
    });

    const resizeObserver = new ResizeObserver(() => {
      fit.fit();
      api.resizePty(sessionId, term.rows, term.cols).catch(() => {});
    });
    resizeObserver.observe(container);

    return () => {
      dataDisposable.dispose();
      unlistenOutput?.();
      unlistenExit?.();
      resizeObserver.disconnect();
      term.dispose();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);

  return <div ref={containerRef} className="terminal-view" />;
});
