<div align="center">

<img src="docs/logo.png" width="96" height="96" alt="TermHub logo" />

# TermHub

**Tile every terminal session in one window — built for running multiple AI coding agents in parallel.**

[![License](https://img.shields.io/badge/license-MIT-lightgrey?style=flat-square)](LICENSE) [![Node](https://img.shields.io/badge/node-18%2B-lightgrey?style=flat-square)](https://nodejs.org) [![Rust](https://img.shields.io/badge/rust-stable-lightgrey?style=flat-square)](https://www.rust-lang.org) [![Platform](https://img.shields.io/badge/platform-macOS-lightgrey?style=flat-square)](#platform-support)

[Features](#features) · [Inter-session messaging](#inter-session-messaging) · [Keyboard Shortcuts](#keyboard-shortcuts) · [Installation](#installation) · [Uninstalling](#uninstalling) · [Building from Source](#building-from-source)

</div>

---

## Overview

<div align="center">

Running several AI coding agents in parallel (Claude Code, Codex, etc.) usually means one OS
terminal window per project, with no single view of what's running where. TermHub replaces that
with one window: every session is a real, independent shell tiled into an even grid, so you can
see and type into several sessions at a glance instead of alt-tabbing between windows.

</div>

<img src="docs/screenshot.png" width="100%" alt="TermHub showing four sessions tiled in a 2x2 grid, with the session sidebar on the left" />

## Features

At a glance:

| | |
|---|---|
| [Tiled terminal grid](#tiled-terminal-grid) | Every session live in an even grid that reflows |
| [Session management](#session-management) | New / close / rename / duplicate, grouped by directory |
| [Rearrange the grid](#rearrange-the-grid) | Long-press a pane and drag to swap positions |
| [Open Recent](#open-recent) | Ctrl+R quick-pick over every folder you've opened |
| [Connect to VPS (SSH)](#connect-to-vps-ssh) | Termius-style picker over saved SSH credentials |
| [Activity indicator](#activity-indicator) | Per-session dot lights up on recent output |
| [Exited-session recovery](#exited-session-recovery) | Dead shells respawn in place on click / keypress |
| [Session persistence](#session-persistence) | Sessions saved to SQLite, reopened on launch |
| [Open in an external terminal](#open-in-an-external-terminal) | Pop a session's folder into iTerm2 / Warp / … |
| [Inter-session messaging](#inter-session-messaging) | Sessions message each other; run commands across sessions |
| [Settings](#settings) | Default shell, preferred external terminal |
| [Token usage dashboard](#token-usage-dashboard) | Per-agent token usage, charts, rate-limit check |

### Tiled terminal grid

- Every open session renders live, laid out in an even NxM grid that reflows as sessions open/close.
- Click a pane to focus it — focused panes get a highlighted border and live cursor.
- Scrollback, mouse selection, and copy/paste are all native, no browser text layer involved (pasting a clipboard image drops in a temp-file path).

### Session management

- New / close / rename / duplicate from the sidebar.
- Sessions are grouped by working directory, each group with a "new session here" shortcut.
- A filter box searches by name or path.

### Rearrange the grid

- Press and hold a pane for a moment (without moving the mouse) to pick it up, then drag onto another pane and release — the two swap grid positions and slide into place.
- A quick press-and-drag still selects text as normal.

### Open Recent

Ctrl+R opens a VSCode-style "Open Recent" quick-pick:

- Type to filter every folder you've ever opened a session in; arrow keys + Enter (or click) opens it as a new session.
- A "Browse for folder…" row at the bottom handles anything not in the list yet (native OS picker).
- Hover/select a row to reveal an ✕ that removes it from the list without closing any session still open there.
- See [Keyboard Shortcuts](#keyboard-shortcuts) for the Ctrl+R tradeoff.

### Connect to VPS (SSH)

A sidebar button (server icon) opens a Termius-style picker over saved SSH credentials — label, host, port, username, and either a password or a saved SSH key.

- Click a credential to connect: a new session spawns running `ssh` straight into it, tiled and managed exactly like any other session (persists, reopens on restart, closes, focuses).
- A spinner shows while it connects, and a connection error surfaces inline instead of closing the picker.
- Password credentials are typed in automatically the moment `ssh`'s own prompt appears.
- Key credentials pick from a vault of saved keys (pasted or imported from a file — TermHub keeps the key's actual content, not a filesystem path, so nothing ever asks you to re-browse for a key file).
- Add credentials or keys from the same picker; an ✕ on each row deletes it.
- This popup only closes via its own Cancel button or a completed connect/save — clicking away or switching apps (common when copying a host/password from elsewhere) leaves it open.

### Activity indicator

A dot next to each session lights up while its shell has produced output recently, so you can tell which agents are still working without switching panes.

### Exited-session recovery

- If a shell process dies (`exit`, a crash, `kill`), its pane shows a dim red border instead of freezing silently, and the sidebar dot turns red.
- Click the pane or just start typing to respawn a fresh shell in the same directory — no need to close and reopen the session.

### Session persistence

Name, working directory, and shell are stored in SQLite. On launch, if any sessions were saved, TermHub asks "Reopen all previous sessions?":

- **Yes** reopens every one as its own tile (staggered slightly to avoid startup-shell races).
- **No** discards the saved list outright (same as closing every session), though those folders stay in the Open Recent picker.

Reconnecting to the original process is out of scope either way — each reopened session starts a fresh shell in the same directory.

### Open in an external terminal

- A per-session button pops that session's folder open in a real, separate terminal app (iTerm2, Warp, Windows Terminal, etc. — auto-detected from what's installed), alongside the built-in terminal.
- Pick a preferred app in Settings, or it falls back to whatever's detected.

### Settings

A gear icon in the sidebar opens a settings panel:

- Override the default shell new sessions spawn (e.g. `/bin/zsh`, `fish`) — leave blank to use `$SHELL`/`COMSPEC`. Only affects sessions created after saving.
- Pick a preferred external terminal app.
- Messaging options live under [Inter-session messaging](#inter-session-messaging).

### Token usage dashboard

Per-agent usage (Claude Code, Codex, Gemini, Aider) with today / last-7-days / all-time totals, a by-session breakdown, and a 14-day chart.

- Tallied by tailing each agent's own local logs/transcripts, no extra instrumentation required.
- Includes an optional API-key-based check against Anthropic's per-key rate-limit headers.

## Inter-session messaging

Lets one session — or an AI coding agent running in it — talk to another: hand off a task,
ask for a command to be run, or leave a note. macOS/Unix only.

Every session's shell starts with:

- the bundled **`termhub-msg`** CLI on its `PATH`
- `TERMHUB_SESSION_ID` and `TERMHUB_SESSION_NAME` in its environment

### CLI

| Command | What it does |
|---|---|
| `termhub-msg list` | List the other open sessions |
| `termhub-msg send <name> <text…>` | Message one session |
| `termhub-msg broadcast <text…>` | Message every other session |
| `termhub-msg inbox` | Read waiting messages — `--wait [--timeout N]` blocks until one arrives |
| `termhub-msg run <session> <cmd…>` | Run a command in another session (opt-in — see below) |

### MCP server (Claude Code agents)

`termhub-msg mcp` runs as an MCP server, so a Claude Code agent recognises messaging natively —
you don't have to explain it each time. Register it once:

```sh
claude mcp add termhub-msg -s user -- /Applications/TermHub.app/Contents/MacOS/termhub-msg mcp
```

**Settings › Messaging** shows this exact line with a copy button.

Each agent then gets these tools:

- `list_sessions`, `send_message`, `broadcast_message`
- `check_inbox`, `wait_for_message`
- `run_in_session`

Once registered, telling an agent *"run this script in session X"* is understood as a TermHub
action with no further setup.

### Run a command in another session

`run_in_session` (MCP tool) / `termhub-msg run <session> <cmd…>` (CLI) types a command into
another session that's sitting at a shell prompt — a local shell or one SSH'd into a VPS — and
returns its combined output and exit code.

- **Opt-in, off by default.** Enable it in **Settings › Messaging**.
- The target must be at an interactive POSIX shell prompt; a REPL or TUI just times out.
- The target's scrollback stays clean: input echo is suppressed while the wrapper is typed and its bookkeeping markers erase themselves, so all that's left is a dim `$ <command>` line (a one-liner verbatim, a multi-line script framed and printed in full) followed by the command's output. Multi-line scripts need `base64` on the target's `PATH`.

### In the app

- A sidebar button toggles a right-edge panel with the live message log.
- Each session row shows an unread-count badge.
- Opt-in **"type incoming messages straight into the terminal"** (Settings › Messaging): an
  arriving message is pasted and submitted at the recipient's prompt — for a keyboard-driven
  agent that isn't polling its inbox. Skipped automatically while that session is blocked in
  `wait_for_message` / `inbox --wait`.

## Keyboard Shortcuts

Matches iTerm2's own bindings apart from Ctrl+R. macOS only for now.

| Shortcut | Action |
|---|---|
| `Cmd+T` | New session |
| `Cmd+W` | Close the active session |
| `Cmd+Shift+]` / `Cmd+Shift+[` | Cycle to the next / previous session |
| `Ctrl+R` | Open the [Open Recent](#features) folder picker |

**Note on Ctrl+R:** it's normally the shell's reverse-i-search. Binding it here means
reverse-i-search no longer reaches the shell in any session — a deliberate tradeoff (chosen over
the conflict-free `Cmd+O`), not a bug.

## Platform Support

| Capability | macOS | Windows | Linux |
|---|---|---|---|
| Session management, persistence, usage tracking, external terminal | ✅ | ✅ | — |
| Terminal keyboard / IME input | ✅ | ✅ code exists, unverified (see below) | — |
| Terminal rendering / builds at all | ✅ | ❌ | — |

**Windows currently doesn't build.**

- `terminal.rs`'s PTY layer only compiles on Unix — `alacritty_terminal`'s Windows `Pty` is ConPTY-backed with a fundamentally different API (no cloneable file handle the way a Unix fd has), which this app's I/O-sharing model doesn't support yet.
- Keyboard/IME input *has* a real implementation for non-macOS (winit's own `KeyboardInput`/`Ime` handling, verified via cross-compilation), but it can't be runtime-tested until the PTY layer is fixed — no Windows machine has actually run this app since the engine rewrite.
- Linux was never in scope.

## Installation

TermHub currently supports **macOS only** (see [Platform Support](#platform-support)).

### Download a prebuilt build

1. Grab the latest `.dmg` from the [Releases page](https://github.com/thangduonghuu/term-hub/releases).
2. Open the `.dmg` and drag **TermHub.app** into `/Applications`.
3. TermHub isn't code-signed, so Gatekeeper blocks it on the first launch. Right-click
   **TermHub.app** → **Open** (instead of double-clicking) and confirm — or allow it afterwards
   via **System Settings → Privacy & Security**. This is only needed once.

If no release is available yet, or you want the latest unreleased changes, build from source
instead (below).

## Building from Source

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (stable toolchain)
- [Node.js](https://nodejs.org/) 18+
- [Tauri platform prerequisites](https://tauri.app/start/prerequisites/)

### Development

```sh
npm install
npm run tauri dev
```

### Release build

```sh
npm run tauri build
```

Produces a release build and installer under `src-tauri/target/release/bundle/`. The app bundle
lands at `src-tauri/target/release/bundle/macos/TermHub.app` — drag it into `/Applications` (or
`cp -R` it there) to install it, same as the prebuilt download above (including the same
one-time Gatekeeper step, since self-built binaries aren't signed either).

Tauri also wraps the bundle into a `.dmg` under `bundle/dmg/`. This step shells out to
`hdiutil`/Finder scripting and can fail in sandboxed or headless environments (CI, some
automation shells) with `error running bundle_dmg.sh`:

- That's just the installer-image step — `TermHub.app` itself still builds successfully and works fine used directly.
- A common cause is macOS blocking the build from sending Apple events to Finder (`Not authorized to send Apple events to Finder. (-1743)`). Grant it under **System Settings → Privacy & Security → Automation** (enable Finder for the terminal app running the build), then re-run `npm run tauri build`.

<details>
<summary><strong>Windows (not yet buildable)</strong></summary>

`cargo check --target x86_64-pc-windows-gnu` currently fails in `terminal.rs` (the PTY layer is
Unix-only) — see [Platform Support](#platform-support) for the exact gap. Once that's fixed, the
process will be: build on a Windows machine (Tauri doesn't cross-compile a Windows installer from
macOS/Linux) with an MSVC toolchain and the Tauri Windows prerequisites installed, using the same
`npm run tauri build` command, producing an `.msi` and/or `.exe` (NSIS) installer under
`bundle/msi/` and `bundle/nsis/`. Unsigned installers will trip Windows SmartScreen on first run
("Windows protected your PC") — click **More info → Run anyway**.

</details>

## Uninstalling

1. Quit TermHub if it's running.
2. Remove the app:

   ```sh
   rm -rf /Applications/TermHub.app
   ```

   (or drag it from `/Applications` to the Trash in Finder).
3. Optional — also remove saved data (sessions, settings, and usage history are stored in a local
   SQLite database, untouched by step 2):

   ```sh
   rm -rf ~/Library/Application\ Support/com.termhub.app
   ```

## License

MIT — see [LICENSE](LICENSE).
