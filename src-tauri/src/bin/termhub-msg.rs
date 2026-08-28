//! `termhub-msg` — CLI for TermHub inter-session messaging.
//!
//! Runs inside a TermHub session's shell, which inherits `TERMHUB_SOCK` and
//! `TERMHUB_SESSION_ID` from the pty env (see `terminal.rs`). Talks to the control server in
//! the running TermHub process (`control.rs`) over a Unix socket: one JSON line out, one JSON
//! line back. See `docs/intersession-messaging-plan.md`.
//!
//! Commands: `list`, `send`, `broadcast`, `inbox` (`--peek` / `--wait` / `--timeout`),
//! `whoami`. A later phase adds an `mcp` subcommand (stdio MCP server).

#[cfg(unix)]
fn main() {
    std::process::exit(unix_impl::run(&std::env::args().skip(1).collect::<Vec<_>>()));
}

#[cfg(not(unix))]
fn main() {
    eprintln!("termhub-msg: inter-session messaging is only supported on Unix so far");
    std::process::exit(1);
}

#[cfg(unix)]
mod unix_impl {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    use serde_json::{json, Value};

    const USAGE: &str = "\
termhub-msg — message other TermHub sessions

usage:
  termhub-msg list                          list open sessions (id, name, unread)
  termhub-msg send <session> <text…>        send a message to a session (name or id)
  termhub-msg broadcast <text…>             send a message to every other open session
  termhub-msg inbox [--peek]                read (and clear) this session's messages
  termhub-msg inbox --wait [--timeout N]    block until a message arrives (N secs, default 60)
  termhub-msg whoami                        show this session's id and name";

    pub fn run(args: &[String]) -> i32 {
        let cmd = args.first().map(String::as_str).unwrap_or("");
        let req = match cmd {
            "list" => json!({ "cmd": "list" }),
            "whoami" => json!({ "cmd": "whoami" }),
            "send" => {
                if args.len() < 3 {
                    eprintln!("termhub-msg: send needs a target and a message\n\n{USAGE}");
                    return 2;
                }
                json!({ "cmd": "send", "args": { "to": args[1], "body": args[2..].join(" ") } })
            }
            "broadcast" => {
                if args.len() < 2 {
                    eprintln!("termhub-msg: broadcast needs a message\n\n{USAGE}");
                    return 2;
                }
                json!({ "cmd": "broadcast", "args": { "body": args[1..].join(" ") } })
            }
            "inbox" => {
                let rest = &args[1..];
                let peek = rest.iter().any(|a| a == "--peek");
                let wait = rest.iter().any(|a| a == "--wait");
                let timeout = rest
                    .iter()
                    .position(|a| a == "--timeout")
                    .and_then(|i| rest.get(i + 1))
                    .and_then(|v| v.parse::<u64>().ok());
                let mut inbox_args = json!({ "peek": peek, "wait": wait });
                if let Some(t) = timeout {
                    inbox_args["timeout_secs"] = json!(t);
                }
                json!({ "cmd": "inbox", "args": inbox_args })
            }
            "-h" | "--help" | "help" => {
                println!("{USAGE}");
                return 0;
            }
            "" => {
                eprintln!("{USAGE}");
                return 2;
            }
            other => {
                eprintln!("termhub-msg: unknown command '{other}'\n\n{USAGE}");
                return 2;
            }
        };

        match call(req) {
            Ok(data) => {
                print_result(cmd, &data);
                0
            }
            Err(e) => {
                eprintln!("termhub-msg: {e}");
                1
            }
        }
    }

    /// One request/response round-trip over the control socket. Fills in `session_id` from the
    /// env TermHub injected, so the server knows which session is calling.
    fn call(mut req: Value) -> Result<Value, String> {
        let sock = std::env::var("TERMHUB_SOCK")
            .map_err(|_| "not inside a TermHub session (TERMHUB_SOCK unset)".to_string())?;
        if let Ok(id) = std::env::var("TERMHUB_SESSION_ID") {
            req["session_id"] = json!(id);
        }
        let stream = UnixStream::connect(&sock)
            .map_err(|e| format!("can't reach TermHub at {sock}: {e}"))?;
        let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
        writer.write_all(req.to_string().as_bytes()).map_err(|e| e.to_string())?;
        writer.write_all(b"\n").map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;

        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).map_err(|e| e.to_string())?;
        let resp: Value = serde_json::from_str(line.trim())
            .map_err(|e| format!("bad response from TermHub: {e}"))?;
        if resp.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(resp.get("data").cloned().unwrap_or(Value::Null))
        } else {
            Err(resp
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string())
        }
    }

    fn print_result(cmd: &str, data: &Value) {
        match cmd {
            "list" => {
                let Some(rows) = data.as_array() else { return };
                if rows.is_empty() {
                    println!("(no sessions)");
                    return;
                }
                for s in rows {
                    let mark = if s["is_current"].as_bool() == Some(true) { "*" } else { " " };
                    let unread = s["unread"].as_i64().unwrap_or(0);
                    let badge = if unread > 0 { format!("  ({unread} unread)") } else { String::new() };
                    println!(
                        "{mark} {:<20} {}{badge}",
                        s["name"].as_str().unwrap_or("?"),
                        s["id"].as_str().unwrap_or("?"),
                    );
                }
            }
            "inbox" => {
                let Some(msgs) = data.as_array() else { return };
                if msgs.is_empty() {
                    println!("(no new messages)");
                    return;
                }
                for m in msgs {
                    let from = m["from_name"].as_str().unwrap_or("outside a session");
                    println!("from {from}:");
                    println!("  {}", m["body"].as_str().unwrap_or(""));
                }
            }
            "send" => {
                println!(
                    "sent to {} (#{})",
                    data["recipient_name"].as_str().unwrap_or("?"),
                    data["message_id"].as_i64().unwrap_or(0),
                );
            }
            "broadcast" => {
                println!(
                    "broadcast to {} session(s)",
                    data["message_count"].as_i64().unwrap_or(0),
                );
            }
            "whoami" => {
                println!(
                    "{}  {}",
                    data["name"].as_str().unwrap_or("?"),
                    data["id"].as_str().unwrap_or("?"),
                );
            }
            _ => {
                println!("{data}");
            }
        }
    }
}
