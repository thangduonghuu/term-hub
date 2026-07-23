<div align="center">

<img src="docs/logo.png" width="96" height="96" alt="TermHub logo" />

# TermHub

Tile every terminal session in one native window — built for running multiple AI coding agents in parallel.

![license](https://img.shields.io/badge/license-MIT-lightgrey?style=flat-square) ![platform](https://img.shields.io/badge/platform-macOS-lightgrey?style=flat-square) ![rust](https://img.shields.io/badge/rust-stable-lightgrey?style=flat-square)

</div>

---

If you run more than a couple of terminal windows at once — especially juggling several AI
coding agents (Claude Code, Codex, etc.) in parallel, one per project — you end up with a mess
of separate OS windows and no single view of what's running where. TermHub puts all of those
sessions in one window instead: every open session renders as a real, independent shell tiled
into an even grid, so you can see and type into several at a glance instead of alt-tabbing
between windows.

## Why a custom engine, not xterm.js

TermHub originally embedded [`xterm.js`](https://xtermjs.org) in a Tauri/WKWebView window. That
approach hit a wall on non-ASCII input: composing Vietnamese (and other IME-based languages) via
Telex was unreliable — the browser's composition events didn't map cleanly onto xterm.js's
keyboard handling, and no amount of app-level patching fully fixed it. Launching each session in
a *real* external terminal app worked (native IME just works), but broke the actual point of the
app — tiling several sessions in one window.

So TermHub's terminal is now a from-scratch native engine instead of a browser-based one:

- **[`winit`](https://github.com/rust-windowing/winit)** — the native window and event loop (no
  `tauri::Builder` at runtime).
- **[`wgpu`](https://wgpu.rs) + [`glyphon`](https://github.com/grovesNL/glyphon)** — GPU-rendered
  text, one draw pass per frame across every tile.
- **[`alacritty_terminal`](https://github.com/alacritty/alacritty)** — PTY spawning and ANSI/VT
  parsing (the same engine that powers Alacritty).
- **On macOS, a hand-written `NSTextInputClient`** (`macos_input_view.rs`) replaces winit's own
  keyboard/IME handling entirely. This was necessary, not cosmetic: winit's macOS IME state
  machine has a confirmed bug where the keystroke immediately following an IME composition commit
  (e.g. the space right after a Vietnamese Telex word) can be silently dropped. The custom view
  fixes that at the root and, as a side effect, gets correct Ctrl+C/D/Z, Escape, Home/End, and
  Page Up/Down forwarding too (AppKit's default key-binding table quietly swallows several of
  these for a plain `NSView`).
- **[`wry`](https://github.com/tauri-apps/wry)** docks the existing React sidebar as a *child
  webview* inside the same native window (`WebViewBuilder::build_as_child`), talking to the Rust
  side over a small hand-rolled IPC shim (`ipc.rs` / `src/lib/ipc.ts`) instead of Tauri's
  `invoke`/`listen`, since Tauri's own IPC only works with a Tauri-owned window.

Net effect: every session is a real PTY with real ANSI/VT handling, real native text input for
any language, and GPU-rendered text — not a sandboxed DOM terminal.

**Currently macOS-only.** The rendering/PTY core (`winit`/`wgpu`/`alacritty_terminal`) is
cross-platform in principle, and `alacritty_terminal` already supports ConPTY on Windows, but the
macOS-specific IME/accessibility work (`macos_input_view.rs`, `macos.rs`) hasn't been ported, and
Linux hasn't been validated at all yet.

## Features

- **Tiled grid of live terminals** — every open session renders at once in a roughly-square grid
  (`ceil(sqrt(n))` columns) that reflows as you open/close sessions. Click into any tile to focus
  it — only the focused tile shows a live cursor and gets keyboard input; it's marked with a
  brighter border.
- **Correct native text input for any language** — including live, correctly-composed Vietnamese
  Telex (the whole reason this engine exists), thanks to the custom `NSTextInputClient`.
- **ANSI colors** — 16-color, 256-color, and 24-bit truecolor, both foreground and background
  (background is needed for more than you'd think — e.g. block-character ASCII-art banners like
  `neofetch`'s rely on it, not just colored text).
- **Cursor shapes and blink** — block/underline/beam, following whatever the running program
  requests (`CSI q`), with app-level blinking as the default the way real terminals do it.
- **Scrollback and mouse-wheel scrolling**, mouse-drag text selection with a highlight, and
  Cmd+C/Cmd+V copy/paste — including pasting a clipboard *image* (e.g. a screenshot), which gets
  written to a temp PNG with its path typed in, matching how iTerm2/Warp/VS Code's terminal
  handle non-text clipboard content.
- **New / close / rename / duplicate / switch** between sessions from the sidebar, with a filter
  box and a live activity dot (lights up briefly whenever a session produces new output).
- **Session persistence & auto-restore** — name, working directory, and shell are stored in
  SQLite; on launch TermHub reconnects a fresh live shell for every saved session (staggered
  ~700ms apart, not all at once — spawning several heavy shell startups in the same instant is a
  real problem for some `.zshrc` setups, e.g. Powerlevel10k's instant-prompt feature can produce
  corrupted output if multiple copies race on its cache file at the same moment).
- **Token usage dashboard** — a toolbar button opens a per-agent usage view (Claude Code, Codex,
  Gemini, Aider) with today / last-7-days / all-time token totals, a by-session breakdown, and a
  14-day by-day chart. Usage is tallied by tailing each agent's own local logs/transcripts (e.g.
  Claude Code's `~/.claude/projects/**/*.jsonl`, Codex's `~/.codex/sessions/**/rollout-*.jsonl`,
  Gemini's `~/.gemini/tmp/**/chats/*.jsonl`, Aider's `.aider.chat.history.md`) — no extra
  instrumentation needed in the agent itself. The Claude Code tab also has an optional
  API-key-based check against Anthropic's per-key rate-limit headers (a separate quota from the
  Claude Pro/Max 5-hour session limit, which isn't exposed by any public API).

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (stable toolchain)
- [Node.js](https://nodejs.org/) 18+
- macOS (Apple Silicon or Intel)

## Development

The sidebar (React/Vite) and the native engine (Rust) run as two separate processes in dev —
there's no `tauri dev` orchestrating both anymore:

```sh
npm install
npm run dev              # starts the Vite dev server on :1420 (the sidebar's content)
```

In another terminal:

```sh
cd src-tauri
cargo run                # builds and launches the native window + engine
```

The native window loads the sidebar from `http://localhost:1420`, so the Vite dev server needs
to already be running first.

## Building a release `.app`

There's no automated bundler wired up yet (`tauri`/`tauri-build` are still listed as
dependencies but unused at runtime — a real `cargo tauri build`-equivalent pipeline is a known
gap). Build and assemble the bundle by hand:

```sh
npm run build             # tsc + vite build, only needed if you serve the built assets
                           # instead of the dev server — see note below
cd src-tauri
cargo build --release
```

`target/release/termhub` is the raw binary. To get a real double-clickable `.app` (needed for
things like reliable keyboard focus and macOS permission prompts to work correctly):

```sh
mkdir -p target/release/bundle/macos/TermHub.app/Contents/MacOS
cp target/release/termhub target/release/bundle/macos/TermHub.app/Contents/MacOS/termhub
# Info.plist with CFBundleExecutable=termhub and CFBundleIdentifier=com.termhub.app goes in
# target/release/bundle/macos/TermHub.app/Contents/

# Ad-hoc sign with a *stable* identifier — important: re-signing with a freshly
# auto-generated identifier on every rebuild (the default for `codesign --sign -` without
# `--identifier`) makes macOS treat every rebuild as a brand new, never-before-seen app, so
# permission grants (Full Disk Access, etc.) silently reset each time.
codesign --force --deep --sign - --identifier com.termhub.app target/release/bundle/macos/TermHub.app

open target/release/bundle/macos/TermHub.app
```

The app isn't signed with a real Apple Developer certificate, so it needs the same one-time
Gatekeeper allowance as any unsigned app (right-click → **Open**, or allow it via **System
Settings → Privacy & Security**). It'll also likely need **Full Disk Access** granted manually
(System Settings → Privacy & Security → Full Disk Access → add it via the **+** button) before it
can access `~/Documents`/`~/Desktop`/`~/Downloads` — unsigned apps generally can't trigger
macOS's normal "app wants access" permission dialog, so the grant has to be added by hand.

## Project layout

- `src/` — React + TypeScript frontend, loaded into the sidebar's child webview:
  - `components/Sidebar.tsx` — session list, filter, rename, new/close/duplicate, activity dot.
  - `components/UsageDashboard.tsx` — per-agent token usage view.
  - `lib/api.ts` — typed wrappers around the IPC commands below.
  - `lib/ipc.ts` — the frontend half of the custom IPC transport (mirrors
    `@tauri-apps/api/core`'s `invoke()` signature so the rest of the frontend didn't need to
    change when Tauri's own IPC was dropped).
- `src-tauri/src/` — Rust backend:
  - `lib.rs` — the `winit` event loop / `ApplicationHandler`: window + `wgpu` surface setup,
    per-tile layout, mouse/keyboard event routing, and the render loop.
  - `terminal.rs` — `alacritty_terminal` PTY/ANSI handling plus the `glyphon`/`wgpu` text and
    selection-highlight/background-color/tile-border rendering.
  - `macos_input_view.rs` — the custom `NSTextInputClient` `NSView` that owns all terminal
    keyboard/IME input on macOS (see "Why a custom engine" above), plus minimal
    `NSAccessibility` support (so tools like screen readers, or other apps that query cursor
    position via the Accessibility API, have something to find).
  - `macos.rs` — other macOS/AppKit interop not covered by `winit`'s cross-platform API (first
    responder handling, screen-coordinate conversion).
  - `ipc.rs` — the hand-rolled IPC router replacing Tauri's `invoke`/`listen`.
  - `db.rs` — SQLite-backed session registry and usage tables (`rusqlite`).
  - `session.rs` — session metadata types and OS defaults (shell, cwd).
  - `usage/` — pluggable adapters (`claude_code.rs`, `codex.rs`, `gemini.rs`, `aider.rs`) that
    tail each agent's local logs/transcripts, plus a background `tracker.rs` poller that persists
    parsed token counts to SQLite.
  - `commands.rs` — the plain Rust functions `ipc.rs` dispatches to.

## License

MIT — see [LICENSE](LICENSE).
