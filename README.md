<div align="center">

<img src="docs/logo.png" width="96" height="96" alt="TermHub logo" />

# TermHub

Tile every terminal session in one window — built for running multiple AI coding agents in parallel.

![license](https://img.shields.io/badge/license-MIT-lightgrey?style=flat-square) ![node](https://img.shields.io/badge/node-18%2B-lightgrey?style=flat-square) ![rust](https://img.shields.io/badge/rust-stable-lightgrey?style=flat-square)

[See the roadmap →](../terminal-manager-prompt.md)

</div>

---

If you run more than a couple of terminal windows at once — especially juggling several AI
coding agents (Claude Code, Codex, etc.) in parallel, one per project — you end up with a mess
of separate OS windows and no single view of what's running where. TermHub puts all of those
sessions in one window instead: every open session renders as a real, independent shell tiled
into an even grid, so you can see and type into several at a glance instead of alt-tabbing
between windows.

It's a cross-platform desktop app (macOS + Windows) built with [Tauri](https://tauri.app)
(Rust backend) + React/TypeScript (frontend). Each session is a real shell process
(zsh/bash/PowerShell/cmd) rendered in-app via [`xterm.js`](https://xtermjs.org), backed by a
real PTY via [`portable-pty`](https://docs.rs/portable-pty) — not a scripted/embedded copy of
iTerm2 or Windows Terminal, which can't be embedded.

![TermHub showing four sessions tiled in a 2x2 grid, one focused with a green border](docs/screenshot.png)

## Features

- **Tiled grid of live terminals** — every running session is shown at once, laid out in an
  even NxM grid (2 sessions → side-by-side, 3–4 → 2×2, 5–6 → 3×2, …) that reflows automatically
  as you open or close sessions, like a tiling window manager. Click into any pane to type — it
  routes to that pane's own shell; the focused pane gets a highlighted border.
- **Per-pane activity indicator** — a status dot in each pane's header flips between
  idle/working/done based on PTY input and output, so you can tell which agents are still
  churning at a glance without switching panes.
- **New / close / rename / switch** between sessions from the sidebar, with a filter box to
  search sessions by name or path. New sessions drop straight into an editable name field so
  you can name them immediately, instead of a generic default label you have to double-click
  later.
- **PTY resize kept in sync** with each terminal view as panes resize.
- **Session persistence & auto-restore** — name, working directory, and shell are stored in
  SQLite, and on launch TermHub reopens every saved session as its own pane, browser-tabs style
  (reconnecting the actual process is out of scope for MVP — each reopened session starts a
  fresh shell in the same working directory).
- **Open in an external terminal** — pick your preferred app (iTerm2, Apple Terminal, Warp,
  Alacritty, WezTerm, Hyper, kitty on macOS; Windows Terminal/PowerShell/Command Prompt on
  Windows — auto-detected from what's actually installed) from the sidebar settings dropdown,
  then hit the `⤢` button on any pane to pop that session's folder open in it. This runs
  alongside the built-in terminal, it doesn't replace it.
- **Token usage dashboard** — a toolbar button opens a per-agent usage view (Claude Code, Codex,
  Gemini, Aider) with today / last-7-days / all-time token totals, a by-session breakdown, and a
  14-day by-day chart, refreshed every few seconds. Usage is tallied by tailing each agent's own
  local logs/transcripts (e.g. Claude Code's `~/.claude/projects/**/*.jsonl`, Codex's
  `~/.codex/sessions/**/rollout-*.jsonl`, Gemini's `~/.gemini/tmp/**/chats/*.jsonl`, Aider's
  `.aider.chat.history.md`) — no extra instrumentation needed in the agent itself. The Claude
  Code tab also has an optional API-key-based check against Anthropic's per-key rate-limit
  headers (a separate quota from the Claude Pro/Max 5-hour session limit, which isn't exposed
  by any public API).

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (stable toolchain)
- [Node.js](https://nodejs.org/) 18+
- Tauri platform prerequisites: https://tauri.app/start/prerequisites/

## Development

```sh
npm install
npm run tauri dev
```

## Building & releasing

```sh
npm run tauri build
```

This produces a release build and platform installers under `src-tauri/target/release/bundle/`.

**macOS**

- The app bundle lands at `src-tauri/target/release/bundle/macos/TermHub.app` — drag it into
  `/Applications` (or `cp -R` it there) to install it like any other Mac app.
- Tauri also tries to wrap that into a `.dmg` under `bundle/dmg/`. This step shells out to
  `hdiutil`/Finder scripting and can fail in sandboxed or headless environments (e.g. CI, some
  automation shells) with `error running bundle_dmg.sh` — that's just the installer-image step;
  `TermHub.app` itself still builds successfully and works fine used directly, no `.dmg` needed
  for personal use.
- The app isn't code-signed (no Apple Developer certificate configured), so macOS Gatekeeper will
  refuse to open it normally on first launch. Right-click the app → **Open** (instead of
  double-clicking) and confirm, or allow it via **System Settings → Privacy & Security**.

**Windows**

- Build on a Windows machine (Tauri doesn't cross-compile a Windows installer from macOS/Linux)
  — same `npm run tauri build` command, with an MSVC toolchain and the Tauri Windows
  prerequisites installed (see the link above).
- Produces an `.msi` and/or `.exe` (NSIS) installer under `bundle/msi/` and `bundle/nsis/`.
- Unsigned installers will similarly trip Windows SmartScreen on first run ("Windows protected
  your PC") — click **More info → Run anyway**.

## Project layout

- `src/` — React + TypeScript frontend:
  - `components/TerminalView.tsx` — xterm.js instance wired to a session's PTY events; tracks
    per-pane activity state.
  - `components/TerminalPane.tsx` — pane chrome (title, close, open-externally, activity dot)
    around a `TerminalView`.
  - `components/Sidebar.tsx` — session list, filter, rename, new/close/reopen, external-app
    setting.
  - `components/UsageDashboard.tsx` — per-agent token usage view (stats, by-session and
    by-day breakdowns, Claude API rate-limit check).
  - `lib/api.ts` — typed wrappers around the Tauri commands below.
- `src-tauri/src/` — Rust backend:
  - `pty_manager.rs` — spawns/tracks PTY processes, streams output as Tauri events.
  - `db.rs` — SQLite-backed session registry and usage tables (`rusqlite`).
  - `external_terminal.rs` — detects installed terminal apps and launches a session's folder
    in one.
  - `usage/` — pluggable adapters (`claude_code.rs`, `codex.rs`, `gemini.rs`, `aider.rs`) that
    tail each agent's local logs/transcripts and a background `tracker.rs` poller that persists
    parsed token counts to SQLite.
  - `commands.rs` — Tauri commands invoked from the frontend.

## License

MIT — see [LICENSE](LICENSE).
# term-hub
