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
| **Activity indicator** | A dot next to each session lights up while its shell has produced output recently, so you can tell which agents are still working without switching panes. |
| **Exited-session recovery** | If a shell process dies (`exit`, a crash, `kill`), its pane shows a dim red border instead of freezing silently, and the sidebar dot turns red. Click the pane or just start typing to respawn a fresh shell in the same directory — no need to close and reopen the session. |
| **Session persistence** | Name, working directory, and shell are stored in SQLite; on launch, TermHub reopens every saved session as its own tile (staggered slightly to avoid startup-shell races). Reconnecting to the original process is out of scope — each reopened session starts a fresh shell in the same directory. |
| **Token usage dashboard** | Per-agent usage (Claude Code, Codex, Gemini, Aider) with today / last-7-days / all-time totals, a by-session breakdown, and a 14-day chart — tallied by tailing each agent's own local logs/transcripts, no extra instrumentation required. Includes an optional API-key-based check against Anthropic's per-key rate-limit headers. |

## Platform Support

| Capability | macOS | Windows / Linux |
|---|---|---|
| Session management, persistence, usage tracking | ✅ | ✅ |
| Terminal rendering | ✅ | ✅ |
| Terminal keyboard / IME input | ✅ | ⚠️ not yet implemented |

Typing into a terminal pane currently requires macOS — support for other platforms is on the
[roadmap](../terminal-manager-prompt.md).

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

- Build on a Windows machine (Tauri doesn't cross-compile a Windows installer from macOS/Linux)
  — same `npm run tauri build` command, with an MSVC toolchain and the Tauri Windows
  prerequisites installed.
- Produces an `.msi` and/or `.exe` (NSIS) installer under `bundle/msi/` and `bundle/nsis/`.
- Unsigned installers will trip Windows SmartScreen on first run ("Windows protected your PC")
  — click **More info → Run anyway**.
- Terminal keyboard input isn't wired up on Windows yet — see [Platform support](#platform-support).

</details>

## License

MIT — see [LICENSE](LICENSE).
