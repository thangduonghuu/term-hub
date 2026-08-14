import { invoke } from "./ipc";

export interface SessionInfo {
  id: string;
  name: string;
  cwd: string;
  shell: string;
  created_at: number;
}

// Mirrors `commands::KeyBinding` in Rust — a raw macOS virtual keycode plus which modifiers
// must be held, not a character (so it's immune to Shift changing what a key produces).
export interface KeyBinding {
  cmd: boolean;
  ctrl: boolean;
  shift: boolean;
  alt: boolean;
  keycode: number;
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
  // Writes straight to a session's pty, regardless of which tile currently has keyboard focus —
  // used by the sidebar's "Resume Claude" button (`claude --continue\r`).
  sendToSession: (id: string, text: string) => invoke<void>("send_to_session", { id, text }),
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
  // Push-to-talk key for voice dictation (see speech.rs) — a raw macOS virtual keycode, one of
  // the curated `[keycode, label]` pairs `getVoicePttKeyOptions` returns (empty on platforms
  // that don't support dictation yet, which is also this section's signal to hide itself in
  // Settings). `getVoicePttKeycode` is null until the user has ever changed it from the
  // built-in default.
  getVoicePttKeyOptions: () => invoke<[number, string][]>("get_voice_ptt_key_options"),
  getVoicePttKeycode: () => invoke<number | null>("get_voice_ptt_keycode"),
  setVoicePttKeycode: (keycode: number) => invoke<void>("set_voice_ptt_keycode", { keycode }),
  // Every user-customizable native keyboard shortcut (Copy, Paste, New/Close/Next/Prev
  // session, Open folder) as `[action id, display label, effective binding]` — the effective
  // binding is already the db override merged with the built-in default (see
  // `commands::get_shortcuts`), so there's always exactly one entry per action, never "unset".
  getShortcuts: () => invoke<[string, string, KeyBinding][]>("get_shortcuts"),
  setShortcut: (action: string, binding: KeyBinding) =>
    invoke<void>("set_shortcut", { action, binding }),
  resetShortcut: (action: string) => invoke<void>("reset_shortcut", { action }),
};
