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

// One line in the right-docked inter-session message log (`MessageLog.tsx`). `from_*` is null
// when the message was sent from a shell outside any session; `to_*` null for a broadcast or a
// since-closed session. The ids let the panel resolve each end to its `#N` and bubble colour.
export interface LogEntry {
  from_id: string | null;
  from_name: string | null;
  to_id: string | null;
  to_name: string | null;
  body: string;
  created_at: number;
}

export type SshAuthMethod = "password" | "key";

// A saved VPS login for the "Connect to VPS" picker (`SshConnect.tsx`) — Termius-style.
// Connecting via `password` auth still spawns a plain `ssh` showing a real login prompt in the
// tile, same as any terminal — `password` gets typed into it automatically the moment that
// prompt appears (see `commands::connect_ssh_session`'s doc comment), rather than being fed to
// `ssh` directly.
export interface SshCredential {
  id: string;
  label: string;
  host: string;
  port: number;
  username: string;
  auth_method: SshAuthMethod;
  password: string | null;
  key_id: string | null;
  created_at: number;
}

// A saved private key in the vault — never carries the key's actual content over IPC (see
// `SshKeySummary` on the Rust side), just enough to show/pick it in the "SSH Key" auth form.
export interface SshKeySummary {
  id: string;
  name: string;
  created_at: number;
}

export const api = {
  listSessions: () => invoke<SessionInfo[]>("list_sessions"),
  createSession: (name?: string, cwd?: string) =>
    invoke<SessionInfo>("create_session", { name, cwd }),
  renameSession: (id: string, name: string) =>
    invoke<void>("rename_session", { id, name }),
  closeSession: (id: string) => invoke<void>("close_session", { id }),
  focusSession: (id: string) => invoke<void>("focus_session", { id }),
  // One-shot: which tile Rust picked as active at startup, for seeding this app's own
  // `activeId` state on mount — `null` if there were no sessions to pick from.
  getActiveSession: () => invoke<string | null>("get_active_session"),
  // Writes straight to a session's pty, regardless of which tile currently has keyboard focus —
  // used by the sidebar's "Resume Claude" button (`claude --continue\r`).
  sendToSession: (id: string, text: string) => invoke<void>("send_to_session", { id, text }),
  // Session id -> unix-epoch ms of its last pty output, for the sidebar's activity dot.
  getActivity: () => invoke<Record<string, number>>("get_activity"),
  // Ids of sessions whose shell process has exited, for the sidebar's dead-session indicator.
  getExitedSessions: () => invoke<string[]>("get_exited_sessions"),
  // Session id -> count of unread inter-session messages, for the sidebar's unread badge.
  getUnreadCounts: () => invoke<Record<string, number>>("get_unread_counts"),
  // `claude mcp add …` snippet for Settings > Messaging (registers the `termhub-msg mcp` server).
  getMcpRegisterCommand: () => invoke<string>("get_mcp_register_command"),
  // Whether a toast pops in the sidebar when this session receives an inter-session message.
  getMessageToastEnabled: () => invoke<boolean>("get_message_toast_enabled"),
  setMessageToastEnabled: (enabled: boolean) =>
    invoke<void>("set_message_toast_enabled", { enabled }),
  // Whether an incoming inter-session message is typed straight into the target session's
  // terminal (for a keyboard-driven agent that doesn't poll its inbox). Opt-in.
  getMessageAutodeliverEnabled: () => invoke<boolean>("get_message_autodeliver_enabled"),
  setMessageAutodeliverEnabled: (enabled: boolean) =>
    invoke<void>("set_message_autodeliver_enabled", { enabled }),
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
  // Slides the right-docked inter-session message log panel in/out (its own webview) and
  // reflows the terminal tiles to the new width.
  toggleMessageLog: () => invoke<void>("toggle_message_log", {}),
  // Recent inter-session messages, oldest-first — the log panel's initial load. Live updates
  // after that arrive as `termhub:message` window events pushed from Rust.
  getMessageLog: () => invoke<LogEntry[]>("get_message_log"),
  // The configured default-shell override for new sessions, or null if unset ($SHELL/COMSPEC
  // is used instead — see `commands::create_session`).
  getDefaultShell: () => invoke<string | null>("get_default_shell"),
  setDefaultShell: (shell: string) => invoke<void>("set_default_shell", { shell }),
  clearDefaultShell: () => invoke<void>("clear_default_shell"),
  // The configured accent color (`#rrggbb`), or null if unset (both the sidebar's own
  // `--accent-color` CSS variable and the native active-tile border fall back to the same
  // built-in gold independently — see `commands::DEFAULT_ACCENT_COLOR`).
  getAccentColor: () => invoke<string | null>("get_accent_color"),
  setAccentColor: (color: string) => invoke<void>("set_accent_color", { color }),
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
  // Reads the system clipboard's text directly (see `commands::read_clipboard_text`'s doc
  // comment) — used by `SshConnect.tsx`'s own Cmd+V handling rather than relying on a plain OS
  // paste reaching a focused HTML input in this app's nonstandard embedded-webview setup.
  readClipboardText: () => invoke<string | null>("read_clipboard_text"),
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
  // "Connect to VPS" picker: saved SSH credentials/keys, and spawning a new session that runs
  // `ssh` straight into one of them.
  listSshCredentials: () => invoke<SshCredential[]>("list_ssh_credentials"),
  createSshCredential: (cred: {
    label: string;
    host: string;
    port: number;
    username: string;
    authMethod: SshAuthMethod;
    password?: string;
    keyId?: string;
  }) => invoke<SshCredential>("create_ssh_credential", cred),
  deleteSshCredential: (id: string) => invoke<void>("delete_ssh_credential", { id }),
  connectSsh: (credentialId: string) => invoke<SessionInfo>("connect_ssh", { credentialId }),
  // Vault of saved private keys — added once (pasted or imported from a file) and picked by
  // name from then on, instead of re-browsing a filesystem path per credential.
  listSshKeys: () => invoke<SshKeySummary[]>("list_ssh_keys"),
  createSshKey: (name: string, content: string) =>
    invoke<SshKeySummary>("create_ssh_key", { name, content }),
  deleteSshKey: (id: string) => invoke<void>("delete_ssh_key", { id }),
  // Native "open a file" dialog that reads the chosen file's content and returns it directly —
  // the path itself is never surfaced or stored (see `commands::read_key_file`'s doc comment).
  readKeyFile: () => invoke<string | null>("read_key_file"),
};
