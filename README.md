<div align="center">

<img src="docs/logo.png" width="96" height="96" alt="TermHub logo" />

# TermHub

**Tile every terminal session in one window — built for running multiple AI coding agents in parallel.**

[![License](https://img.shields.io/badge/license-MIT-lightgrey?style=flat-square)](LICENSE) [![Node](https://img.shields.io/badge/node-18%2B-lightgrey?style=flat-square)](https://nodejs.org) [![Rust](https://img.shields.io/badge/rust-stable-lightgrey?style=flat-square)](https://www.rust-lang.org) [![Platform](https://img.shields.io/badge/platform-macOS-lightgrey?style=flat-square)](#platform-support)

[Roadmap](../terminal-manager-prompt.md) · [Features](#features) · [Getting Started](#getting-started)

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
| **Keyboard shortcuts** | Cmd+T new session, Cmd+W close the active one, Cmd+Shift+]/Cmd+Shift+[ to cycle next/previous — matching iTerm2's bindings. macOS only for now. |
| **Activity indicator** | A dot next to each session lights up while its shell has produced output recently, so you can tell which agents are still working without switching panes. |
| **Exited-session recovery** | If a shell process dies (`exit`, a crash, `kill`), its pane shows a dim red border instead of freezing silently, and the sidebar dot turns red. Click the pane or just start typing to respawn a fresh shell in the same directory — no need to close and reopen the session. |
| **Session persistence** | Name, working directory, and shell are stored in SQLite; on launch, TermHub reopens every saved session as its own tile (staggered slightly to avoid startup-shell races). Reconnecting to the original process is out of scope — each reopened session starts a fresh shell in the same directory. |
| **Open in an external terminal** | A per-session button pops that session's folder open in a real, separate terminal app (iTerm2, Warp, Windows Terminal, etc. — auto-detected from what's installed), alongside the built-in terminal. Pick a preferred app in Settings, or it falls back to whatever's detected. |
| **Settings** | A gear icon in the sidebar opens a settings panel: override the default shell new sessions spawn (e.g. `/bin/zsh`, `fish`) — leave blank to use `$SHELL`/`COMSPEC`, only affects sessions created after saving — and pick a preferred external terminal app. |
| **Token usage dashboard** | Per-agent usage (Claude Code, Codex, Gemini, Aider) with today / last-7-days / all-time totals, a by-session breakdown, and a 14-day chart — tallied by tailing each agent's own local logs/transcripts, no extra instrumentation required. Includes an optional API-key-based check against Anthropic's per-key rate-limit headers. |

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
rewrite. Linux was never in scope (see the [roadmap](../terminal-manager-prompt.md)'s Platform
Scope). Details in the roadmap doc.

## Getting Started

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (stable toolchain)
- [Node.js](https://nodejs.org/) 18+
- [Tauri platform prerequisites](https://tauri.app/start/prerequisites/)

### Development

```sh
npm install
npm run tauri dev
```

## Building & Releasing

```sh
npm run tauri build
```

Produces a release build and platform installers under `src-tauri/target/release/bundle/`.

<details>
<summary><strong>macOS</strong></summary>

- The app bundle lands at `src-tauri/target/release/bundle/macos/TermHub.app` — drag it into
  `/Applications` (or `cp -R` it there) to install it like any other Mac app.
- Tauri also wraps the bundle into a `.dmg` under `bundle/dmg/`. This step shells out to
  `hdiutil`/Finder scripting and can fail in sandboxed or headless environments (CI, some
  automation shells) with `error running bundle_dmg.sh` — that's just the installer-image step;
  `TermHub.app` itself still builds successfully and works fine used directly.
- The app isn't code-signed, so Gatekeeper will refuse to open it on first launch. Right-click →
  **Open** (instead of double-clicking) and confirm, or allow it via **System Settings → Privacy
  & Security**.

</details>

<details>
<summary><strong>Windows</strong></summary>

**Doesn't build yet** — `cargo check --target x86_64-pc-windows-gnu` fails in `terminal.rs`
(the PTY layer is Unix-only). See [Platform support](#platform-support) and the
[roadmap](../terminal-manager-prompt.md) for the exact gap. The rest of this section describes
the intended process once that's fixed.

- Build on a Windows machine (Tauri doesn't cross-compile a Windows installer from macOS/Linux)
  — same `npm run tauri build` command, with an MSVC toolchain and the Tauri Windows
  prerequisites installed.
- Produces an `.msi` and/or `.exe` (NSIS) installer under `bundle/msi/` and `bundle/nsis/`.
- Unsigned installers will trip Windows SmartScreen on first run ("Windows protected your PC")
  — click **More info → Run anyway**.

</details>

## License

MIT — see [LICENSE](LICENSE).
