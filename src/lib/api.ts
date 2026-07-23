import { invoke } from "./ipc";

export interface SessionInfo {
  id: string;
  name: string;
  cwd: string;
  shell: string;
  created_at: number;
}

export interface SessionUsage {
  session_id: string | null;
  session_name: string;
  agent: string;
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
  agent: string;
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

export interface ClaudeLimits {
  limits: [string, string][];
}

export const api = {
  listSessions: () => invoke<SessionInfo[]>("list_sessions"),
  createSession: (name?: string, cwd?: string) =>
    invoke<SessionInfo>("create_session", { name, cwd }),
  renameSession: (id: string, name: string) =>
    invoke<void>("rename_session", { id, name }),
  closeSession: (id: string) => invoke<void>("close_session", { id }),
  focusSession: (id: string) => invoke<void>("focus_session", { id }),
  // Session id -> unix-epoch ms of its last pty output, for the sidebar's activity dot.
  getActivity: () => invoke<Record<string, number>>("get_activity"),
  getDefaultCwd: () => invoke<string>("get_default_cwd"),
  getUsageSummary: () => invoke<UsageSummary>("get_usage_summary"),
  hasAnthropicApiKey: () => invoke<boolean>("has_anthropic_api_key"),
  setAnthropicApiKey: (key: string) => invoke<void>("set_anthropic_api_key", { key }),
  clearAnthropicApiKey: () => invoke<void>("clear_anthropic_api_key"),
  checkClaudeLimits: () => invoke<ClaudeLimits>("check_claude_limits"),
};
