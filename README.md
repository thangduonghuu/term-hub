<div align="center">

<img src="docs/logo.png" width="96" height="96" alt="TermHub logo" />

# TermHub

**Tile every terminal session in one window — built for running multiple AI coding agents in parallel.**

[![License](https://img.shields.io/badge/license-MIT-lightgrey?style=flat-square)](LICENSE) [![Node](https://img.shields.io/badge/node-18%2B-lightgrey?style=flat-square)](https://nodejs.org) [![Rust](https://img.shields.io/badge/rust-stable-lightgrey?style=flat-square)](https://www.rust-lang.org) [![Platform](https://img.shields.io/badge/platform-macOS-lightgrey?style=flat-square)](#platform-support)

[Features](#features) · [Keyboard Shortcuts](#keyboard-shortcuts) · [Installation](#installation) · [Uninstalling](#uninstalling) · [Building from Source](#building-from-source)

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

| | |
|---|---|
| **Tiled terminal grid** | Every open session renders live, laid out in an even NxM grid that reflows as sessions open/close. Click a pane to focus it — focused panes get a highlighted border and live cursor. Scrollback, mouse selection, and copy/paste (including pasting a clipboard image as a temp-file path) are all native, no browser text layer involved. |
| **Session management** | New / close / rename / duplicate from the sidebar. Sessions are grouped by working directory with a per-group "new session here" shortcut, plus a filter box to search by name or path. |
| **Open Recent** | A sidebar button (and Ctrl+R) opens a VSCode-style "Open Recent" quick-pick: type to filter every folder you've ever opened a session in, arrow keys + Enter (or click) to open it as a new session, and a "Browse for folder…" row at the bottom for anything not in the list yet (native OS picker). Hover/select a row to reveal an ✕ that removes it from the list without closing any session still open there. See [Keyboard Shortcuts](#keyboard-shortcuts) for the Ctrl+R tradeoff. |
| **Activity indicator** | A dot next to each session lights up while its shell has produced output recently, so you can tell which agents are still working without switching panes. |
| **Exited-session recovery** | If a shell process dies (`exit`, a crash, `kill`), its pane shows a dim red border instead of freezing silently, and the sidebar dot turns red. Click the pane or just start typing to respawn a fresh shell in the same directory — no need to close and reopen the session. |
| **Session persistence** | Name, working directory, and shell are stored in SQLite. On launch, if any sessions were saved, TermHub asks "Reopen all previous sessions?" — Yes reopens every one as its own tile (staggered slightly to avoid startup-shell races); No discards the saved list outright (same as closing every session), though those folders stay in the Open Recent picker. Reconnecting to the original process is out of scope either way — each reopened session starts a fresh shell in the same directory. |
| **Open in an external terminal** | A per-session button pops that session's folder open in a real, separate terminal app (iTerm2, Warp, Windows Terminal, etc. — auto-detected from what's installed), alongside the built-in terminal. Pick a preferred app in Settings, or it falls back to whatever's detected. |
| **Settings** | A gear icon in the sidebar opens a settings panel: override the default shell new sessions spawn (e.g. `/bin/zsh`, `fish`) — leave blank to use `$SHELL`/`COMSPEC`, only affects sessions created after saving — and pick a preferred external terminal app. |
| **Token usage dashboard** | Per-agent usage (Claude Code, Codex, Gemini, Aider) with today / last-7-days / all-time totals, a by-session breakdown, and a 14-day chart — tallied by tailing each agent's own local logs/transcripts, no extra instrumentation required. Includes an optional API-key-based check against Anthropic's per-key rate-limit headers. |

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

**Windows currently doesn't build.** `terminal.rs`'s PTY layer only compiles on Unix —
`alacritty_terminal`'s Windows `Pty` is ConPTY-backed with a fundamentally different API (no
cloneable file handle the way a Unix fd has), which this app's I/O-sharing model doesn't
support yet. Keyboard/IME input *has* a real implementation for non-macOS (winit's own
`KeyboardInput`/`Ime` handling, verified via cross-compilation), but it can't be runtime-tested
until the PTY layer is fixed — no Windows machine has actually run this app since the engine
rewrite. Linux was never in scope.

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
automation shells) with `error running bundle_dmg.sh` — that's just the installer-image step;
`TermHub.app` itself still builds successfully and works fine used directly. A common cause is
macOS blocking the build from sending Apple events to Finder
(`Not authorized to send Apple events to Finder. (-1743)`); grant it under **System Settings →
Privacy & Security → Automation** (enable Finder for the terminal app running the build), then
re-run `npm run tauri build`.

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
