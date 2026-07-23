// Replacement for @tauri-apps/api/core's `invoke`, matching its call signature exactly so
// api.ts and everything above it (Sidebar.tsx, UsageDashboard.tsx) don't need to change.
//
// Why this exists: the sidebar now runs as a `wry` webview embedded as a *child* of our own
// native window (not a Tauri-owned window), which forfeits Tauri's built-in IPC. The Rust
// side (`src-tauri/src/ipc.rs`) receives `window.ipc.postMessage(json)` messages and replies
// by evaluating `window.__ipcResolve(id, {ok, data|error})` back into this page.

interface PendingCall {
  resolve: (value: unknown) => void;
  reject: (reason: unknown) => void;
}

declare global {
  interface Window {
    ipc: { postMessage: (message: string) => void };
    __ipcResolve: (id: number, result: { ok: boolean; data?: unknown; error?: string }) => void;
  }
}

const pending = new Map<number, PendingCall>();
let nextId = 1;

window.__ipcResolve = (id, result) => {
  const call = pending.get(id);
  if (!call) return;
  pending.delete(id);
  if (result.ok) call.resolve(result.data);
  else call.reject(new Error(result.error ?? "unknown IPC error"));
};

export function invoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  return new Promise((resolve, reject) => {
    const id = nextId++;
    pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
    window.ipc.postMessage(JSON.stringify({ id, cmd, args }));
  });
}
