import { invoke } from "@tauri-apps/api/core";

export interface SessionInfo {
  id: string;
  name: string;
  cwd: string;
  shell: string;
  created_at: number;
  running: boolean;
}

export interface SessionUsage {
  session_id: string | null;
  session_name: string;
  tokens_in: number;
  tokens_out: number;
}

export interface AgentUsage {
  agent: string;
  tokens_in: number;
  tokens_out: number;
}

export interface DayUsage {
  day: string;
  tokens_in: number;
  tokens_out: number;
}

export interface UsageSummary {
  per_session: SessionUsage[];
  per_agent: AgentUsage[];
  per_day: DayUsage[];
  total_tokens_in: number;
  total_tokens_out: number;
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
  getUsageSummary: () => invoke<UsageSummary>("get_usage_summary"),
};
