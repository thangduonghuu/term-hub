import { invoke } from "@tauri-apps/api/core";

export interface SessionInfo {
  id: string;
  name: string;
  cwd: string;
  shell: string;
  created_at: number;
  running: boolean;
}

export const api = {
  listSessions: () => invoke<SessionInfo[]>("list_sessions"),
  createSession: (name?: string, cwd?: string) =>
    invoke<SessionInfo>("create_session", { name, cwd }),
  reopenSession: (id: string) => invoke<SessionInfo>("reopen_session", { id }),
  writePty: (id: string, data: string) => invoke<void>("write_pty", { id, data }),
  resizePty: (id: string, rows: number, cols: number) =>
    invoke<void>("resize_pty", { id, rows, cols }),
  renameSession: (id: string, name: string) =>
    invoke<void>("rename_session", { id, name }),
  closeSession: (id: string) => invoke<void>("close_session", { id }),
  getDefaultCwd: () => invoke<string>("get_default_cwd"),
  listTerminalApps: () => invoke<string[]>("list_terminal_apps"),
  openExternalTerminal: (app: string, cwd: string) =>
    invoke<void>("open_external_terminal", { app, cwd }),
};
