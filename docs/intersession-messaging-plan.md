# TermHub inter-session messaging — implementation plan

Lets a Claude Code agent in one TermHub session send messages to a Claude Code agent in
another. Built **into** TermHub; the only new artifact is a small `termhub` binary shipped in
the same repo that Claude Code talks to as an MCP server.

## Status

- **Phase 1 — done.** `control.rs` Unix-socket server (`list` / `send` / `inbox` / `whoami`),
  `messages` table + `Db` methods, env injection in `terminal.rs::spawn`
  (`TERMHUB_SOCK` / `TERMHUB_SESSION_ID` / `TERMHUB_SESSION_NAME` + `PATH`), and the
  `termhub-msg` CLI (`src-tauri/src/bin/termhub-msg.rs`). Socket is `/tmp/termhub-<user>.sock`
  (chmod `0600`) — not the app data dir, which is too long for `sun_path`. 4 tests in
  `control.rs`. CLI binary name is `termhub-msg` for now; rename to `termhub` in Phase 3 (see
  Open decision #4).
- **Message-log panel — done** (brought forward from Phase 4's push mechanism). A second child
  webview docked to the right strip (`MessageLog.tsx`, loaded via `?panel=messages`), toggled
  from a sidebar button (`AppEvent::ToggleMessageLog`), `LOG_PANEL_WIDTH` = 300, tiles reflow.
  `control.rs`'s `send` fires `AppEvent::MessageLogged` via a `notify` callback → forwarded to
  the panel as a `termhub:message` DOM event; history loads on mount via `get_message_log`
  (`Db::recent_messages`).
- **Phase 2 — done.** `broadcast` command (`control.rs` + `termhub-msg broadcast`, fans out to
  one row per other session); blocking `inbox --wait [--timeout N]` (500 ms poll loop in
  `dispatch`, default 60 s, cap 600, peek-then-real-take so a mid-wait arrival is consumed
  once); `read_at` tracking and `purge_messages_for` on `delete_session` were already in.
  `get_unread_counts` IPC command polled by `App.tsx`, driving a per-session unread badge in
  the sidebar. `Cargo.toml` gained `default-run = "termhub"` (the two-binary clash the plan's
  Open decision #4 flagged). 8 `control.rs` tests. Condvar wake for `--wait` still deferred to
  Phase 4.
- **Phase 3 — done.** `termhub-msg mcp` — a hand-rolled stdio JSON-RPC 2.0 server
  (newline-delimited, not `Content-Length`): `initialize` / `ping` / `tools/list` /
  `tools/call`, five tools (`list_sessions`, `send_message`, `broadcast_message`,
  `check_inbox`, `wait_for_message`), each one round-trip through the same `call()` socket
  helper the CLI uses. `get_mcp_register_command` IPC returns the absolute-path
  `claude mcp add termhub-msg -- <path> mcp` line; a new **Messaging** section in
  `SettingsPanel.tsx` shows it with a copy button (hidden on non-Unix, like Voice). 5
  `mcp::tests`. The `get_message_settings` / `set_message_settings` enable+nudge settings are
  Phase 4, not here.
- **Phase 4 — mostly done.**
  - *Arrival toast:* `control.rs` fires `AppEvent::MessageNudge` once per recipient (from `send`
    and each `broadcast` target) with a length-capped `preview`; `lib.rs` forwards it to the
    sidebar webview as `termhub:message-toast` (gated on `intersession_toast`, default on) and
    `App.tsx` shows an auto-dismissing banner. Checkbox in Settings > Messaging (`get`/
    `set_message_toast_enabled`).
  - *`TERMHUB_TOKEN` hardening:* new `session_tokens` table (own table, never rides into
    `SessionMeta`); `App::spawn_session` mints a fresh `Uuid` per pty and injects it as
    `TERMHUB_TOKEN` alongside `TERMHUB_SESSION_ID`; `termhub-msg` forwards it; `control.rs`'s
    `check_token` requires it to match for `inbox` / `whoami` (lenient only when no token is on
    record — a pre-feature session, or one mid-spawn). `send` / `broadcast` / `list` stay open
    (sender-spoofing is the accepted risk). `delete_session` / `clear_sessions` purge tokens.
    +1 `control.rs` test (9 total).
  - **Still open:** Condvar wake for `inbox --wait` (the 500 ms poll is fine) and the Windows
    named-pipe backend (the app doesn't build on Windows at all yet).
- **Phase 5 — auto-deliver into the terminal — done.** Revisits the Phase 4 "still open" idle
  *pty* nudge, now that typing into the recipient is the *wanted* behaviour for a
  keyboard-driven agent. New `intersession_autodeliver` setting (Settings › Messaging,
  **opt-in**, absent = off — unlike the toast, this drives the target's input). When on,
  `send` / `broadcast` fire `AppEvent::MessageInject` instead of `MessageNudge`: `lib.rs`
  bracketed-pastes `[termhub-msg from <who>] <body>` + a trailing `\r` into the target
  session's pty (`Terminal::paste` + `write`), so a Claude Code agent there picks the message
  up as a submitted prompt without ever polling `check_inbox`. The inject also clears the
  target's unread rows (`take_inbox`) — it's been delivered by typing, so the sidebar badge
  shouldn't double-count — and the arrival toast is skipped for that recipient
  (`maybe_inject` returns whether it injected; callers gate the nudge on it). **Suppressed
  while the target is parked in `inbox --wait`:** a process-global `waiting_sessions` set,
  populated by a `WaitGuard` (RAII — clears on every early `return` and on panic) around the
  `--wait` poll loop, tells `maybe_inject` to fall through to the normal unread path, so a
  `wait_for_message` agent never gets the same message both typed in *and* returned from the
  wait. `get` / `set_message_autodeliver_enabled` IPC (`ipc.rs` + `commands.rs`) and
  `api.ts`; checkbox in `SettingsPanel.tsx`. +2 `control.rs` tests
  (`autodeliver_on_injects_instead_of_nudging`, `autodeliver_skipped_while_target_is_waiting`),
  13 total. **Verified live:** #2 → #1 `send` auto-typed into #1's terminal with #1's inbox
  left empty; #1 → #2 reply while #2 sat in `inbox --wait` was returned from the wait (not
  injected), and a following `inbox --peek` showed it already consumed.
- **Phase 6 — run a command in another session — done.** A `run` control command types a
  base64-wrapped one-liner into a target session's pty and returns its combined stdout/stderr
  + exit code to the caller. The wrapper brackets execution output with `__THUB_<nonce>_S` /
  `__THUB_<nonce>_E<code>` markers (expanded from a shell var, so the shell's own echo of the
  typed line can't match them); `App` types it in via `AppEvent::RunInSession` and arms an
  `OutputCapture` on the target's `TerminalSession`, whose pty reader thread answers the
  `run` handler's `mpsc` reply once the end marker lands (or `TimedOut` past the deadline, or
  `Truncated` past a 2 MiB cap). `strip_ansi` in `terminal.rs` flattens the captured bytes.
  Assumes the target sits at an interactive POSIX shell prompt (local or `ssh`) with `base64`
  on PATH — a REPL/TUI just times out, harmlessly. **Opt-in:** the `intersession_run` setting
  (`get`/`set_message_run_enabled` IPC, `api.ts`, `SettingsPanel.tsx` checkbox), off by
  default; every run is also written to the message-log panel as `$ <command>`. Exposed as
  the `run_in_session` MCP tool and `termhub-msg run <session> [--timeout N] <cmd…>` (which
  exits with the remote command's own status). +4 `control.rs` tests, 17 total.

---

## 1. Architecture

```
 Session A pty                          TermHub process (main)                Session B pty
┌───────────────┐                     ┌──────────────────────────┐          ┌───────────────┐
│ claude        │                     │  control.rs              │          │ claude        │
│  └─ MCP tool  │  stdio JSON-RPC     │   UnixListener on        │          │  └─ MCP tool  │
│     call      │───► termhub mcp ───►│   termhub.sock           │          │     check_    │
│               │     (child proc)    │   • resolve name→id      │          │     inbox ────┼─► termhub mcp
│               │     line JSON over  │   • Db: messages table   │          └───────────────┘   │
└───────────────┘     UnixStream      │   • proxy → AppEvent     │                  ▲            │
        ▲                             │        ::Notify          │                  │            │
        │  env injected at spawn:     │   • toast / idle nudge   │──────────────────┘            │
        │  TERMHUB_SOCK               │                          │   reply over same socket ◄────┘
        │  TERMHUB_SESSION_ID         └──────────┬───────────────┘
        │  TERMHUB_SESSION_NAME                  │
        │  PATH += <dir of termhub>              ▼
        └───────────────────────────────  sqlite: messages
```

**Four components:**

| # | Component | New/changed | Role |
|---|-----------|-------------|------|
| 1 | `src-tauri/src/control.rs` | new | Unix-socket server in the TermHub process; owns message dispatch + mailbox logic |
| 2 | `messages` table + `Db` methods | changed `db.rs` | store of record for messages (survives restart, like `sessions`) |
| 3 | env injection | changed `terminal.rs::spawn` | give every session its identity + how to reach the socket + `termhub` on `PATH` |
| 4 | `src-tauri/src/bin/termhub.rs` | new `[[bin]]` | dual-mode: `termhub mcp` (stdio MCP server) and `termhub <subcommand>` (human CLI); both thin proxies to the socket |

Plus wiring in `lib.rs` / `ipc.rs` / `commands.rs` and a Settings section + toast + unread badge in the frontend.

**Why a socket + child binary and not the existing webview IPC:** `ipc.rs` only serves
`window.ipc.postMessage` from the sidebar webview. A shell process inside a pty has no route
to it. A Unix domain socket is the same mechanism tmux uses (`/tmp/tmux-<uid>/default`).

---

## 2. Data model

Add to `db.rs`'s `execute_batch` in `Db::open` (idempotent `CREATE TABLE IF NOT EXISTS`, same
pattern as the other tables):

```sql
CREATE TABLE IF NOT EXISTS messages (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    from_session TEXT,                 -- sender session id; NULL = sent from outside a session
    to_session   TEXT NOT NULL,        -- recipient session id
    body         TEXT NOT NULL,
    created_at   INTEGER NOT NULL,     -- unix seconds
    read_at      INTEGER               -- NULL until consumed by check_inbox (non-peek)
);
CREATE INDEX IF NOT EXISTS idx_messages_inbox
    ON messages (to_session, read_at, id);
```

Broadcast is expanded at send time into one row per live recipient (keeps the read model
trivial). Messages to a now-deleted session are cleaned up by a
`DELETE FROM messages WHERE to_session = ?1` added to `Db::delete_session`.

**New `Db` methods:**

```rust
fn insert_message(&self, from: Option<&str>, to: &str, body: &str, ts: i64) -> rusqlite::Result<i64>;
fn take_inbox(&self, session: &str, peek: bool, ts: i64) -> rusqlite::Result<Vec<Message>>; // marks read_at unless peek
fn unread_counts(&self) -> rusqlite::Result<HashMap<String, i64>>; // for the sidebar badge
fn purge_messages_for(&self, session: &str) -> rusqlite::Result<()>;
```

`Message` struct in `message.rs` (new):
`{ id: i64, from_session: Option<String>, from_name: Option<String>, body: String, created_at: i64 }`
— `from_name` filled by a `LEFT JOIN sessions`.

---

## 3. Wire protocol (CLI/MCP ↔ control server)

Line-delimited JSON over the Unix socket, one request → one response per connection (no
framing beyond `\n`).

```jsonc
// request
{ "v": 1, "session_id": "<uuid|null>", "token": "<opaque|null>",
  "cmd": "send", "args": { "to": "backend", "body": "run the integration tests" } }

// response
{ "ok": true,  "data": { "message_id": 42, "recipients": ["<uuid>"] } }
{ "ok": false, "error": "no session named or id'd \"backend\"" }
```

**Commands:** `whoami`, `list`, `send {to, body}`, `broadcast {body}`,
`inbox {peek?: bool, wait?: bool, timeout_secs?: number}`,
`run {to, command, timeout_secs?: number}` (Phase 6 — `data` is
`{exit_code, output, truncated}`; gated on the `intersession_run` setting).

- `to` resolves as: exact id → exact name (case-insensitive) → error listing candidates.
- `inbox` with `wait: true`: control server polls `take_inbox(peek=true)` every 500 ms up to
  `timeout_secs` (default 60, cap 600); on hit, does the real non-peek take and returns.
  (Condvar optimisation deferred to Phase 4.)
- `session_id`/`token` come from the caller's env. Missing `session_id` is allowed for
  `list`/`send` from a plain shell; required for `inbox`/`whoami`.

---

## 4. MCP surface (`termhub mcp`)

Stdio JSON-RPC 2.0. Hand-rolled — the needed subset is `initialize`, `tools/list`,
`tools/call` (~150–200 LOC, no SDK). `rmcp` is an option but a heavy dep for four tools.

| Tool | Args | Returns |
|------|------|---------|
| `list_sessions` | – | `[{id, name, cwd, is_current, unread}]` |
| `send_message` | `to` (name or id), `body` | `{message_id, recipients}` |
| `check_inbox` | `peek?` (bool) | `[{id, from_name, from_id, body, created_at}]`; marks read unless `peek` |
| `wait_for_message` | `timeout_seconds?` | next message, or `{timed_out: true}` |
| `broadcast_message` | `body` | `{message_id_count}` |
| `run_in_session` (Phase 6) | `to`, `command`, `timeout_seconds?` | `{exit_code, output, truncated}` — runs `command` in a session sitting at a shell prompt |

Each call → one socket round-trip. `session_id` is read once from `$TERMHUB_SESSION_ID` at startup.

---

## 5. Implementation detail, component by component

### 5.1 `control.rs` (new)

```rust
pub struct ControlState {
    pub db: Arc<Db>,
    pub proxy: EventLoopProxy<AppEvent>,
    pub sock_path: PathBuf,
}

pub fn spawn(state: ControlState) -> std::io::Result<()> {
    let _ = std::fs::remove_file(&state.sock_path);          // clear stale socket
    let listener = UnixListener::bind(&state.sock_path)?;
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let state = state.clone_handles();
            std::thread::spawn(move || handle_conn(conn, &state)); // thread per conn, like ipc.rs
        }
    });
    Ok(())
}
```

- `handle_conn`: read one line, `serde_json::from_str::<Req>`, `match req.cmd { … }`, write one JSON line.
- `send`: resolve `to` against `db.list_sessions()`; `db.insert_message(...)`;
  `proxy.send_event(AppEvent::Notify { session_id, from_name, preview })`.
- `list`: `db.list_sessions()` + `db.unread_counts()`; mark `is_current` by comparing to `req.session_id`.
- `inbox`: `db.take_inbox(session, peek, now)`, or the poll loop when `wait`.
- Uses only `Send` handles (`Arc<Db>`, `EventLoopProxy`) — no `App` access needed.

Started from `run()` in `lib.rs` right after `proxy` is created (line ~1946), before
`event_loop.run_app`:

```rust
let sock_path = app_data_dir().join("termhub.sock");
let _ = control::spawn(control::ControlState {
    db: db.clone(), proxy: proxy.clone(), sock_path: sock_path.clone(),
});
```

On exit (`WindowEvent::CloseRequested` / end of `run`), `std::fs::remove_file(sock_path)`.

### 5.2 Session identity — `terminal.rs::spawn`

At the `env` HashMap build (currently `terminal.rs:280`), add:

```rust
env.insert("TERMHUB_SOCK".into(),          sock_path.display().to_string());
env.insert("TERMHUB_SESSION_ID".into(),    id.clone());
env.insert("TERMHUB_SESSION_NAME".into(),  name.to_string());
// put the `termhub` binary (shipped beside the app) on PATH
if let Ok(exe) = std::env::current_exe() {
    if let Some(dir) = exe.parent() {
        let path = std::env::var("PATH").unwrap_or_default();
        env.insert("PATH".into(), format!("{}:{}", dir.display(), path));
    }
}
```

`spawn` gains two params: `name: &str`, `sock_path: &Path`. Threaded through:

- `App::spawn_session(...)` signature +`name`, +`sock_path` (add `App.sock_path: PathBuf`,
  set in `App::new` / from `run()`).
- `AppEvent::SpawnSession { id, cwd, shell }` → `+ name: String`.
- `ipc.rs` `"create_session"` handler: include `info.meta.name`.
- The startup reconnect path (`pending_reconnects`, `SessionMeta`) already carries `.name`.

### 5.3 `bin/termhub.rs` (new `[[bin]]`)

`Cargo.toml`:

```toml
[[bin]]
name = "termhub"
path = "src/bin/termhub.rs"
```

(The GUI stays `main.rs`; set `default-run` or rename one bin to avoid the name clash — see
Open decision #4.)

```rust
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("mcp")       => mcp::serve(),                        // stdio JSON-RPC loop
        Some("send")      => cli::send(&args[1], &args[2..].join(" ")),
        Some("inbox")     => cli::inbox(flag("--wait"), flag_val("--timeout")),
        Some("list")      => cli::list(),
        Some("broadcast") => cli::broadcast(&args[1..].join(" ")),
        Some("whoami")    => cli::whoami(),
        _ => { eprintln!("{USAGE}"); std::process::exit(2); }
    }
}
```

`transport.rs`: `fn call(req: Req) -> Result<Value>` — connect `UnixStream::connect($TERMHUB_SOCK)`,
write `serde_json::to_string(&req) + "\n"`, read a line, parse. Fills `session_id`/`token` from env.

`mcp.rs`: LSP-style stdio framing (`Content-Length` headers, one JSON-RPC object per message).
Handle `initialize` → capabilities `{tools:{}}`; `tools/list` → the static schema array;
`tools/call` → map to `transport::call` and wrap the result as
`{content:[{type:"text", text:<json>}]}`.

### 5.4 `lib.rs` — `AppEvent::Notify`

```rust
Notify { session_id: String, from_name: Option<String>, preview: String },
```

Handler in `user_event`:

1. Always: forward a toast to the sidebar webview — reuse the `termhub:voice-error` pattern
   (`webview.evaluate_script("window.dispatchEvent(new CustomEvent('termhub:toast', {detail: …}))")`).
2. Nudge (setting-gated, default *toast only*): if `intersession_nudge == "inject"` **and** the
   session looks idle (`self.activity` timestamp for `session_id` older than ~2 s),
   `term.write("\r\x1b[2m[termhub] message from {from} — run check_inbox\x1b[0m\r")`.
   Never inject into a session with recent output.

### 5.5 `ipc.rs` / `commands.rs`

New commands (frontend ↔ TermHub, for Settings + badge):

| cmd | commands.rs fn | notes |
|-----|----------------|-------|
| `get_message_settings` | reads `settings` keys `intersession_enabled`, `intersession_nudge` | |
| `set_message_settings` | writes them | |
| `get_unread_counts` | `db.unread_counts()` | polled by `App.tsx` like `get_activity` |
| `get_mcp_register_command` | returns the exact `claude mcp add …` string with the resolved binary path | for the copy-button in Settings |

### 5.6 Frontend

- **`src/lib/api.ts`**: add the four methods above + types.
- **`src/App.tsx`**:
  - `useEffect` polling `api.getUnreadCounts()` on `ACTIVITY_POLL_MS` → `unreadBySession` state,
    passed to `Sidebar`.
  - listener for `termhub:toast` → transient banner, reuse `voice-error-banner` styling /
    auto-dismiss timer (generalise the class to `.toast-banner`).
- **`src/components/Sidebar.tsx`**: a ✉ badge with count on a session row when `unread > 0` —
  next to the existing activity dot / exited indicator.
- **`src/components/SettingsPanel.tsx`**: new "Inter-session messaging" section — enable toggle,
  nudge mode (`Toast only` / `Toast + terminal note` / `Off`), and a read-only
  `claude mcp add …` field with a copy button. Hidden entirely if the platform has no socket
  support (Windows until Phase 4), same way the voice section hides when
  `get_voice_ptt_key_options` is empty.

### 5.7 Claude Code registration

Phase 3 ships manual: Settings shows

```
claude mcp add termhub -- termhub mcp
```

(`termhub` is already on `PATH` inside every session via 5.2, so this works verbatim from any
session's shell.)

Phase 4 optional auto-register: a setting "Register the termhub MCP server in new sessions"
that, on `SpawnSession`, writes a project-scoped `.mcp.json` in the session cwd if absent, or
runs `claude mcp add --scope local …` via a one-shot injected command.

---

## 6. Phased rollout

### Phase 1 — plumbing (send + list over CLI) · ~1 day

- `control.rs` socket server; `messages` table + `insert_message` / `take_inbox`; env
  injection (5.2); `AppEvent::SpawnSession` gains `name`.
- `termhub` bin with `list`, `send`, `inbox` (no `--wait`, no MCP).
- **Verify:** open two sessions; in A run `termhub send B "hello"`; in B run `termhub inbox`
  → prints the message. `termhub list` shows both with ids/names. `cargo build && cargo clippy`
  clean.

### Phase 2 — inbox semantics · ~½ day

- `--wait`/`--timeout`, `read_at` tracking, `broadcast`, `purge_messages_for` in
  `delete_session`.
- `get_unread_counts` IPC + sidebar ✉ badge + `App.tsx` polling.
- **Verify:** `termhub inbox --wait` in B blocks, unblocks when A sends; badge appears/clears;
  closing a session drops its messages.

### Phase 3 — MCP · ~1 day

- `termhub mcp` stdio server; five tool schemas; binary-path resolution for the register
  snippet.
- Settings "Inter-session messaging" section with the `claude mcp add` line.
- **Verify:** `claude mcp add termhub -- termhub mcp` in two sessions; ask Claude in A
  "message the session named B and tell it to run the tests"; Claude in B: "check your inbox"
  → sees it. `npm run build` clean.

### Phase 4 — push + polish · ~1 day

- `AppEvent::Notify` toast; idle-gated pty nudge + setting; `TERMHUB_TOKEN` hardening; Condvar
  wake for `--wait`; Windows named-pipe backend (`interprocess` crate) behind `#[cfg]`.

**Total ≈ 3–4 days.**

---

## 7. Testing

- **Unit** (`db.rs`): `insert_message` → `take_inbox` peek vs consume; `unread_counts`;
  name→id resolution ambiguity.
- **Integration** (`control.rs` `#[cfg(test)]`): bind server on a `tempfile` socket +
  in-memory `Db`, drive it with a raw `UnixStream`, assert `send`/`inbox`/`broadcast`/`wait`
  (with a short timeout).
- **MCP** (`bin/termhub.rs`): feed canned `initialize` / `tools/list` / `tools/call` JSON on
  stdin, assert stdout frames.
- **Manual matrix:** CLI path (Phase 1/2), MCP path (Phase 3), nudge idle vs busy (Phase 4),
  auto-deliver inject vs `inbox --wait` suppression (Phase 5), session delete mid-conversation,
  TermHub restart with unread messages pending.
- CI: `cargo build`, `cargo clippy -- -D warnings` for the new modules, `npm run build`.

---

## 8. Security / threat model

- Unix socket in the `0700` app-data dir → same-user only. Consistent with the existing
  sqlite/`termhub.sqlite` trust boundary.
- `TERMHUB_SESSION_ID` is injected by TermHub but a co-located process could spoof it. Phase 4
  adds `TERMHUB_TOKEN` (random per session, stored in a `session_tokens` table, checked
  server-side) so a message can't be *read* for another session without its token. Spoofing
  the *sender* id is low-impact (worst case: a misattributed message) — accepted.
- No network listener, ever. Windows named pipe (Phase 4) uses a default-DACL private pipe.

---

## 9. Open decisions (pick before Phase 1)

1. **Message store:** sqlite (recommended — survives restart, matches `sessions`) vs in-memory
   only. Plan assumes sqlite.
2. **MCP impl:** hand-rolled JSON-RPC (recommended, zero deps) vs `rmcp` SDK.
3. **Default nudge behaviour:** toast-only (recommended) vs toast+inject.
4. **Binary naming:** GUI stays `termhub`, CLI becomes `termhub-msg`? or GUI → `termhub-app`,
   CLI → `termhub`? (Plan assumes CLI = `termhub`, GUI bin renamed.)
5. **Broadcast semantics:** fan-out to rows now (recommended) vs a single `to_session="*"` row
   resolved at read time.

---

## 10. File change summary

**New:** `src-tauri/src/control.rs`, `src-tauri/src/bin/termhub.rs` (+ `mcp.rs`,
`transport.rs`, `cli.rs` submodules), `src-tauri/src/message.rs`.

**Changed:** `src-tauri/Cargo.toml` (`[[bin]]`, maybe `interprocess` in P4), `db.rs`
(table + ~4 methods + purge on delete), `terminal.rs` (`spawn` params + env), `lib.rs`
(`AppEvent::SpawnSession.name`, `AppEvent::Notify`, `App.sock_path`, control server start in
`run()`, socket cleanup), `ipc.rs` + `commands.rs` (4 commands), `session.rs` (thread `name`
where needed), `src/lib/api.ts`, `src/App.tsx`, `src/components/Sidebar.tsx`,
`src/components/SettingsPanel.tsx`.
