//! Unix-domain-socket control server for inter-session messaging.
//!
//! See `docs/intersession-messaging-plan.md`. A small `termhub-msg` CLI — run from inside a
//! session's shell, which inherits `TERMHUB_SOCK` / `TERMHUB_SESSION_ID` from the pty env (see
//! `terminal.rs`) — connects here to send messages to, and read messages from, other sessions.
//!
//! Wire format: one line of JSON per request, one line of JSON per response, one exchange per
//! connection. A thread per connection, same shape as `ipc.rs`'s dispatch. Everything it needs
//! is `Send` (`Arc<Db>`), so it never has to reach into `App`.
//!
//! Commands: `whoami`, `list`, `send`, `broadcast`, `inbox` (with `peek` / `wait` /
//! `timeout_secs`). `inbox` with `wait` polls `take_inbox` every 500 ms until a message lands
//! or the timeout elapses — a thread-per-connection blocking read, no `AppEvent` involved.
//! Still to come (Phase 3+): the `termhub-msg mcp` stdio server and a toast/idle-nudge push.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::Db;
use crate::session::SessionMeta;
use crate::AppEvent;

pub struct ControlState {
    pub db: Arc<Db>,
    pub sock_path: PathBuf,
    /// Called on the sending thread each time a message is delivered — `run()` wires this to
    /// `EventLoopProxy::send_event` so the log-panel webview updates live. Kept as a plain
    /// callback (not the proxy itself) so this module stays independent of `winit` and its
    /// tests don't need an event loop.
    pub notify: Box<dyn Fn(AppEvent) + Send + Sync>,
}

#[derive(Deserialize)]
struct Req {
    #[serde(default)]
    session_id: Option<String>,
    /// The caller's `TERMHUB_TOKEN` (see `terminal.rs`) — required to match before `inbox` /
    /// `whoami` will act for `session_id`. Not needed for `list` / `send` / `broadcast`.
    #[serde(default)]
    token: Option<String>,
    cmd: String,
    #[serde(default)]
    args: Value,
}

/// Binds the control socket and serves it on a background thread. A bind failure (stale socket
/// held by a crashed instance, permissions, a non-Unix quirk) is logged and swallowed —
/// inter-session messaging just stays unavailable rather than taking the app down with it.
pub fn spawn(state: ControlState) {
    // A prior run that didn't exit cleanly leaves the socket file behind; `bind` then fails
    // with EADDRINUSE until it's removed.
    let _ = std::fs::remove_file(&state.sock_path);
    let listener = match UnixListener::bind(&state.sock_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "termhub: control socket bind failed ({e}); inter-session messaging disabled"
            );
            return;
        }
    };
    // The socket sits in world-writable `/tmp`; make it owner-only so another local user can't
    // connect and read/inject messages. (Phase 4 adds a per-session token on top.)
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&state.sock_path, std::fs::Permissions::from_mode(0o600));
    let state = Arc::new(state);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                if let Err(e) = handle_conn(conn, &state) {
                    eprintln!("termhub: control connection error: {e}");
                }
            });
        }
    });
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn handle_conn(conn: UnixStream, state: &ControlState) -> std::io::Result<()> {
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut writer = conn;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let resp = match serde_json::from_str::<Req>(&line) {
        Ok(req) => match dispatch(&req, state) {
            Ok(data) => json!({ "ok": true, "data": data }),
            Err(e) => json!({ "ok": false, "error": e }),
        },
        Err(e) => json!({ "ok": false, "error": format!("bad request: {e}") }),
    };
    writer.write_all(resp.to_string().as_bytes())?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn dispatch(req: &Req, state: &ControlState) -> Result<Value, String> {
    let db = &state.db;
    match req.cmd.as_str() {
        "whoami" => {
            let id = req.session_id.as_deref().ok_or("not running inside a TermHub session")?;
            check_token(db, id, req.token.as_deref())?;
            let meta = db.get_session(id).map_err(|_| "session not found".to_string())?;
            Ok(json!({ "id": meta.id, "name": meta.name, "cwd": meta.cwd }))
        }
        "list" => {
            let sessions = db.list_sessions().map_err(|e| e.to_string())?;
            let unread = db.unread_counts().map_err(|e| e.to_string())?;
            let me = req.session_id.as_deref();
            let out: Vec<Value> = sessions
                .iter()
                .map(|s| {
                    json!({
                        "id": s.id,
                        "name": s.name,
                        "cwd": s.cwd,
                        "is_current": Some(s.id.as_str()) == me,
                        "unread": unread.get(&s.id).copied().unwrap_or(0),
                    })
                })
                .collect();
            Ok(json!(out))
        }
        "send" => {
            let to: String = field(&req.args, "to")?;
            let body: String = field(&req.args, "body")?;
            if body.trim().is_empty() {
                return Err("empty message body".into());
            }
            let sessions = db.list_sessions().map_err(|e| e.to_string())?;
            let target = resolve(&sessions, &to)?;
            if Some(target.id.as_str()) == req.session_id.as_deref() {
                return Err("that's this session — pick a different one".into());
            }
            let ts = now();
            let mid = db
                .insert_message(req.session_id.as_deref(), &target.id, &body, ts)
                .map_err(|e| e.to_string())?;
            let from_name = req
                .session_id
                .as_deref()
                .and_then(|id| sessions.iter().find(|s| s.id == id))
                .map(|s| s.name.clone());
            // Feed the live message-log panel (`MessageLog.tsx`) and the recipient's toast.
            (state.notify)(AppEvent::MessageLogged {
                from_name: from_name.clone(),
                to_name: target.name.clone(),
                body: body.clone(),
                ts,
            });
            (state.notify)(AppEvent::MessageNudge {
                to_id: target.id.clone(),
                from_name,
                preview: preview(&body),
            });
            Ok(json!({ "message_id": mid, "recipient": target.id, "recipient_name": target.name }))
        }
        "broadcast" => {
            let body: String = field(&req.args, "body")?;
            if body.trim().is_empty() {
                return Err("empty message body".into());
            }
            let me = req.session_id.as_deref();
            let sessions = db.list_sessions().map_err(|e| e.to_string())?;
            let ts = now();
            let mut recipients = Vec::new();
            for s in &sessions {
                if Some(s.id.as_str()) == me {
                    continue;
                }
                db.insert_message(me, &s.id, &body, ts).map_err(|e| e.to_string())?;
                recipients.push(s.id.clone());
            }
            if recipients.is_empty() {
                return Err("no other sessions to broadcast to".into());
            }
            let from_name = me
                .and_then(|id| sessions.iter().find(|s| s.id == id))
                .map(|s| s.name.clone());
            (state.notify)(AppEvent::MessageLogged {
                from_name: from_name.clone(),
                to_name: format!("everyone ({} sessions)", recipients.len()),
                body: body.clone(),
                ts,
            });
            for to_id in &recipients {
                (state.notify)(AppEvent::MessageNudge {
                    to_id: to_id.clone(),
                    from_name: from_name.clone(),
                    preview: preview(&body),
                });
            }
            Ok(json!({ "message_count": recipients.len(), "recipients": recipients }))
        }
        "inbox" => {
            let id = req.session_id.as_deref().ok_or("not running inside a TermHub session")?;
            check_token(db, id, req.token.as_deref())?;
            let peek = req.args.get("peek").and_then(Value::as_bool).unwrap_or(false);
            let wait = req.args.get("wait").and_then(Value::as_bool).unwrap_or(false);
            if wait {
                // Poll for a message up to `timeout_secs` (default 60, capped at 600). Each tick
                // is a cheap `peek` read; the real (possibly consuming) read happens once, only
                // after something's actually there — so a message landing mid-wait is taken
                // exactly once.
                let timeout =
                    req.args.get("timeout_secs").and_then(Value::as_u64).unwrap_or(60).min(600);
                let deadline = Instant::now() + Duration::from_secs(timeout);
                loop {
                    let waiting = db.take_inbox(id, true, now()).map_err(|e| e.to_string())?;
                    if !waiting.is_empty() {
                        let msgs = db.take_inbox(id, peek, now()).map_err(|e| e.to_string())?;
                        return serde_json::to_value(msgs).map_err(|e| e.to_string());
                    }
                    if Instant::now() >= deadline {
                        return Ok(json!([]));
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
            let msgs = db.take_inbox(id, peek, now()).map_err(|e| e.to_string())?;
            serde_json::to_value(msgs).map_err(|e| e.to_string())
        }
        other => Err(format!("unknown command: {other}")),
    }
}

/// Confirms the caller holds `session`'s `TERMHUB_TOKEN` before it's allowed to read that
/// session's mailbox. Lenient when no token is on record — a session predating this feature, or
/// one still mid-spawn — but a token that *is* recorded has to match exactly.
fn check_token(db: &Db, session: &str, presented: Option<&str>) -> Result<(), String> {
    match db.session_token(session).map_err(|e| e.to_string())? {
        Some(expected) if presented != Some(expected.as_str()) => {
            Err("bad or missing TERMHUB_TOKEN for this session".into())
        }
        _ => Ok(()),
    }
}

/// A one-line, length-capped snippet of a message body for the arrival toast — the full text
/// still lands in the inbox and the log panel.
fn preview(body: &str) -> String {
    const MAX_CHARS: usize = 120;
    let one_line = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > MAX_CHARS {
        format!("{}…", one_line.chars().take(MAX_CHARS).collect::<String>())
    } else {
        one_line
    }
}

fn field<T: serde::de::DeserializeOwned>(args: &Value, key: &str) -> Result<T, String> {
    let raw = args.get(key).cloned().ok_or(format!("missing arg: {key}"))?;
    serde_json::from_value(raw).map_err(|e| format!("bad arg {key}: {e}"))
}

/// Resolves a `send` target given as either an exact session id or a session name
/// (case-insensitive). A name shared by several open sessions is rejected with their ids so the
/// caller can disambiguate.
fn resolve<'a>(sessions: &'a [SessionMeta], q: &str) -> Result<&'a SessionMeta, String> {
    if let Some(s) = sessions.iter().find(|s| s.id == q) {
        return Ok(s);
    }
    let matches: Vec<&SessionMeta> =
        sessions.iter().filter(|s| s.name.eq_ignore_ascii_case(q)).collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("no session named or id'd \"{q}\"")),
        many => Err(format!(
            "\"{q}\" matches {} sessions — use an id: {}",
            many.len(),
            many.iter().map(|s| s.id.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn test_db() -> (Arc<Db>, std::path::PathBuf) {
        let path = std::env::temp_dir()
            .join(format!("termhub-control-test-{}.sqlite", uuid::Uuid::new_v4()));
        let db = Arc::new(Db::open(&path).unwrap());
        for (id, name) in [("id-a", "alice"), ("id-b", "bob")] {
            db.insert_session(&SessionMeta {
                id: id.into(),
                name: name.into(),
                cwd: "/tmp".into(),
                shell: String::new(),
                created_at: 0,
            })
            .unwrap();
        }
        (db, path)
    }

    fn req(session_id: Option<&str>, cmd: &str, args: Value) -> Req {
        Req { session_id: session_id.map(str::to_string), token: None, cmd: cmd.into(), args }
    }

    #[test]
    fn send_by_name_then_inbox_consumes_once() {
        let (db, path) = test_db();
        let state = ControlState { db: db.clone(), sock_path: "/unused".into(), notify: Box::new(|_| {}) };

        let sent = dispatch(
            &req(Some("id-a"), "send", json!({ "to": "bob", "body": "ping" })),
            &state,
        )
        .unwrap();
        assert_eq!(sent["recipient"], "id-b");

        let inbox = dispatch(&req(Some("id-b"), "inbox", json!({})), &state).unwrap();
        assert_eq!(inbox.as_array().unwrap().len(), 1);
        assert_eq!(inbox[0]["body"], "ping");
        assert_eq!(inbox[0]["from_name"], "alice");

        // consumed — a second read is empty
        let again = dispatch(&req(Some("id-b"), "inbox", json!({})), &state).unwrap();
        assert!(again.as_array().unwrap().is_empty());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn send_fires_notify_with_endpoint_names() {
        let (db, path) = test_db();
        let (tx, rx) = std::sync::mpsc::channel();
        let state = ControlState {
            db,
            sock_path: "/unused".into(),
            notify: Box::new(move |ev| tx.send(ev).unwrap()),
        };
        dispatch(&req(Some("id-a"), "send", json!({ "to": "bob", "body": "ship it" })), &state)
            .unwrap();

        match rx.try_recv().unwrap() {
            AppEvent::MessageLogged { from_name, to_name, body, .. } => {
                assert_eq!(from_name.as_deref(), Some("alice"));
                assert_eq!(to_name, "bob");
                assert_eq!(body, "ship it");
            }
            _ => panic!("expected AppEvent::MessageLogged"),
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn inbox_peek_leaves_message_unread() {
        let (db, path) = test_db();
        let state = ControlState { db, sock_path: "/unused".into(), notify: Box::new(|_| {}) };
        dispatch(&req(Some("id-a"), "send", json!({ "to": "id-b", "body": "hi" })), &state).unwrap();

        let peek = dispatch(&req(Some("id-b"), "inbox", json!({ "peek": true })), &state).unwrap();
        assert_eq!(peek.as_array().unwrap().len(), 1);
        let read = dispatch(&req(Some("id-b"), "inbox", json!({})), &state).unwrap();
        assert_eq!(read.as_array().unwrap().len(), 1);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn send_rejects_self_unknown_and_lists_current() {
        let (db, path) = test_db();
        let state = ControlState { db, sock_path: "/unused".into(), notify: Box::new(|_| {}) };

        assert!(dispatch(
            &req(Some("id-a"), "send", json!({ "to": "alice", "body": "x" })),
            &state
        )
        .unwrap_err()
        .contains("this session"));

        assert!(dispatch(
            &req(Some("id-a"), "send", json!({ "to": "nobody", "body": "x" })),
            &state
        )
        .unwrap_err()
        .contains("no session"));

        let list = dispatch(&req(Some("id-a"), "list", json!({})), &state).unwrap();
        let rows = list.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let alice = rows.iter().find(|r| r["name"] == "alice").unwrap();
        assert_eq!(alice["is_current"], true);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn broadcast_fans_out_to_every_other_session() {
        let (db, path) = test_db();
        db.insert_session(&SessionMeta {
            id: "id-c".into(),
            name: "carol".into(),
            cwd: "/tmp".into(),
            shell: String::new(),
            created_at: 0,
        })
        .unwrap();
        let state =
            ControlState { db: db.clone(), sock_path: "/unused".into(), notify: Box::new(|_| {}) };

        let out = dispatch(
            &req(Some("id-a"), "broadcast", json!({ "body": "standup in 5" })),
            &state,
        )
        .unwrap();
        assert_eq!(out["message_count"], 2);

        for who in ["id-b", "id-c"] {
            let inbox = dispatch(&req(Some(who), "inbox", json!({})), &state).unwrap();
            assert_eq!(inbox[0]["body"], "standup in 5");
        }
        // the sender doesn't get its own broadcast
        let mine = dispatch(&req(Some("id-a"), "inbox", json!({})), &state).unwrap();
        assert!(mine.as_array().unwrap().is_empty());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn inbox_wait_returns_a_message_already_waiting() {
        let (db, path) = test_db();
        let state = ControlState { db, sock_path: "/unused".into(), notify: Box::new(|_| {}) };
        dispatch(&req(Some("id-a"), "send", json!({ "to": "id-b", "body": "now" })), &state).unwrap();

        let got = dispatch(
            &req(Some("id-b"), "inbox", json!({ "wait": true, "timeout_secs": 2 })),
            &state,
        )
        .unwrap();
        assert_eq!(got[0]["body"], "now");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn inbox_enforces_token_once_one_is_recorded() {
        let (db, path) = test_db();
        db.set_session_token("id-b", "secret-b").unwrap();
        let state =
            ControlState { db: db.clone(), sock_path: "/unused".into(), notify: Box::new(|_| {}) };
        dispatch(&req(Some("id-a"), "send", json!({ "to": "id-b", "body": "hi" })), &state).unwrap();

        // no token, then wrong token → rejected, message left untouched
        let mut call = req(Some("id-b"), "inbox", json!({}));
        assert!(dispatch(&call, &state).unwrap_err().contains("TERMHUB_TOKEN"));
        call.token = Some("wrong".into());
        assert!(dispatch(&call, &state).unwrap_err().contains("TERMHUB_TOKEN"));

        // right token → delivered
        call.token = Some("secret-b".into());
        let got = dispatch(&call, &state).unwrap();
        assert_eq!(got[0]["body"], "hi");

        // a session with no token on record stays lenient (id-a was never given one)
        assert!(dispatch(&req(Some("id-a"), "whoami", json!({})), &state).is_ok());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn inbox_wait_times_out_empty() {
        let (db, path) = test_db();
        let state = ControlState { db, sock_path: "/unused".into(), notify: Box::new(|_| {}) };

        let got = dispatch(
            &req(Some("id-b"), "inbox", json!({ "wait": true, "timeout_secs": 1 })),
            &state,
        )
        .unwrap();
        assert!(got.as_array().unwrap().is_empty());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn end_to_end_over_the_socket() {
        let (db, db_path) = test_db();
        // Short path — a Unix socket must fit in `sun_path` (~104 bytes); the macOS temp dir
        // alone is already close to that.
        let sock_path = std::path::PathBuf::from(format!(
            "/tmp/th-test-{}-{}.sock",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        spawn(ControlState { db, sock_path: sock_path.clone(), notify: Box::new(|_| {}) });

        // give the listener a moment to bind
        for _ in 0..50 {
            if sock_path.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let roundtrip = |req: Value| -> Value {
            let mut s = UnixStream::connect(&sock_path).unwrap();
            s.write_all(format!("{req}\n").as_bytes()).unwrap();
            let mut buf = String::new();
            s.read_to_string(&mut buf).unwrap();
            serde_json::from_str(buf.trim()).unwrap()
        };

        let sent = roundtrip(json!({
            "session_id": "id-a", "cmd": "send", "args": { "to": "bob", "body": "over the wire" }
        }));
        assert_eq!(sent["ok"], true);

        let inbox = roundtrip(json!({ "session_id": "id-b", "cmd": "inbox", "args": {} }));
        assert_eq!(inbox["ok"], true);
        assert_eq!(inbox["data"][0]["body"], "over the wire");

        let bad = roundtrip(json!({ "cmd": "nonsense" }));
        assert_eq!(bad["ok"], false);

        let _ = std::fs::remove_file(sock_path);
        let _ = std::fs::remove_file(db_path);
    }
}
