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
  // Ids of sessions whose shell process has exited, for the sidebar's dead-session indicator.
  getExitedSessions: () => invoke<string[]>("get_exited_sessions"),
  getDefaultCwd: () => invoke<string>("get_default_cwd"),
  // Native OS folder-browse dialog — null if the user cancels. Used by the "Browse…" row in the
  // Open Recent picker, for folders that aren't in the MRU list yet.
  pickFolder: () => invoke<string | null>("pick_folder"),
  // Folders previously opened as a session, most-recent first (VSCode's "Open Recent").
  listRecentFolders: () => invoke<string[]>("list_recent_folders"),
  removeRecentFolder: (path: string) => invoke<void>("remove_recent_folder", { path }),
  // Widens/narrows the sidebar webview to full-window while any full-screen modal (usage
  // dashboard, settings) is open/closed — their centered-overlay CSS only has as much viewport
  // to work with as the webview itself.
  setOverlayOpen: (open: boolean) => invoke<void>("set_overlay_open", { open }),
  // The configured default-shell override for new sessions, or null if unset ($SHELL/COMSPEC
  // is used instead — see `commands::create_session`).
  getDefaultShell: () => invoke<string | null>("get_default_shell"),
  setDefaultShell: (shell: string) => invoke<void>("set_default_shell", { shell }),
  clearDefaultShell: () => invoke<void>("clear_default_shell"),
  // Terminal apps installed on this machine (iTerm2, Warp, Windows Terminal, etc.) that a
  // session's folder can be popped open in as an alternative to the built-in native terminal.
  listTerminalApps: () => invoke<string[]>("list_terminal_apps"),
  getPreferredTerminalApp: () => invoke<string | null>("get_preferred_terminal_app"),
  setPreferredTerminalApp: (app: string) =>
    invoke<void>("set_preferred_terminal_app", { app }),
  openExternalTerminal: (app: string, cwd: string) =>
    invoke<void>("open_external_terminal", { app, cwd }),
  // Whether the one-time "try Lumen" sidebar promo has already been dismissed/acted on.
  hasSeenLumenPrompt: () => invoke<boolean>("has_seen_lumen_prompt"),
  markLumenPromptSeen: () => invoke<void>("mark_lumen_prompt_seen"),
  openUrl: (url: string) => invoke<void>("open_url", { url }),
  getUsageSummary: () => invoke<UsageSummary>("get_usage_summary"),
  hasAnthropicApiKey: () => invoke<boolean>("has_anthropic_api_key"),
  setAnthropicApiKey: (key: string) => invoke<void>("set_anthropic_api_key", { key }),
  clearAnthropicApiKey: () => invoke<void>("clear_anthropic_api_key"),
  checkClaudeLimits: () => invoke<ClaudeLimits>("check_claude_limits"),
};
