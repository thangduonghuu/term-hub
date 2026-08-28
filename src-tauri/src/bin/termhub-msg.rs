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
  termhub-msg list                          list open sessions (#num, name, id, unread)
  termhub-msg send <session> <text…>        send a message to a session (#num, name, or id)
  termhub-msg broadcast <text…>             send a message to every other open session
  termhub-msg inbox [--peek]                read (and clear) this session's messages
  termhub-msg inbox --wait [--timeout N]    block until a message arrives (N secs, default 60)
  termhub-msg whoami                        show this session's id and name
  termhub-msg mcp                           run as a stdio MCP server (for `claude mcp add`)";

    pub fn run(args: &[String]) -> i32 {
        let cmd = args.first().map(String::as_str).unwrap_or("");
        // `mcp` is the odd one out: a long-lived stdio JSON-RPC server for Claude Code, not a
        // one-shot socket round-trip like every other subcommand.
        if cmd == "mcp" {
            return mcp::serve();
        }
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
        if let Ok(token) = std::env::var("TERMHUB_TOKEN") {
            req["token"] = json!(token);
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
                        "{mark} #{:<3} {:<20} {}{badge}",
                        s["num"].as_i64().unwrap_or(0),
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

    /// `termhub-msg mcp` — a minimal stdio MCP server so Claude Code can send/receive
    /// inter-session messages as tool calls. Register it from inside any session with:
    ///
    /// ```text
    /// claude mcp add termhub-msg -- termhub-msg mcp
    /// ```
    ///
    /// Newline-delimited JSON-RPC 2.0 on stdin/stdout (MCP's stdio transport — no
    /// `Content-Length` framing). Hand-rolled: the surface is just `initialize`, `tools/list`,
    /// `tools/call`, `ping`. Every tool is one round-trip through `super::call` to the same
    /// control socket the CLI uses, so `$TERMHUB_SOCK` / `$TERMHUB_SESSION_ID` must be in the
    /// env (they are, inside a session shell).
    mod mcp {
        use std::io::{BufRead, Write};

        use serde_json::{json, Value};

        const PROTOCOL_VERSION: &str = "2025-06-18";

        pub fn serve() -> i32 {
            let stdin = std::io::stdin();
            let mut stdout = std::io::stdout();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                let msg: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    // Can't recover an id from unparseable input — nothing to reply to.
                    Err(_) => continue,
                };
                if let Some(resp) = handle(&msg) {
                    if writeln!(stdout, "{resp}").is_err() || stdout.flush().is_err() {
                        break;
                    }
                }
            }
            0
        }

        /// `Some(response)` for a request (`id` present), `None` for a notification.
        fn handle(msg: &Value) -> Option<Value> {
            let id = msg.get("id").cloned();
            let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
            // Notifications (`notifications/initialized`, `notifications/cancelled`, …) carry no
            // id and get no reply.
            id.as_ref()?;
            let id = id.unwrap();

            let result: Result<Value, (i64, String)> = match method {
                "initialize" => Ok(json!({
                    "protocolVersion": msg
                        .get("params")
                        .and_then(|p| p.get("protocolVersion"))
                        .and_then(Value::as_str)
                        .unwrap_or(PROTOCOL_VERSION),
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "termhub-msg", "version": env!("CARGO_PKG_VERSION") },
                })),
                "ping" => Ok(json!({})),
                "tools/list" => Ok(json!({ "tools": tool_schemas() })),
                "tools/call" => {
                    let params = msg.get("params").cloned().unwrap_or(Value::Null);
                    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                    let args = params.get("arguments").cloned().unwrap_or(json!({}));
                    match call_tool(name, &args) {
                        Ok(data) => Ok(json!({
                            "content": [{
                                "type": "text",
                                "text": serde_json::to_string_pretty(&data).unwrap_or_default(),
                            }],
                        })),
                        // Tool-level failure is reported in-band with `isError`, not as a
                        // protocol error — that's the MCP convention.
                        Err(e) => Ok(json!({
                            "content": [{ "type": "text", "text": e }],
                            "isError": true,
                        })),
                    }
                }
                other => Err((-32601, format!("method not found: {other}"))),
            };

            Some(match result {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err((code, message)) => json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": code, "message": message },
                }),
            })
        }

        fn call_tool(name: &str, args: &Value) -> Result<Value, String> {
            let str_arg = |k: &str| {
                args.get(k)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| format!("missing string argument: {k}"))
            };
            let req = match name {
                "list_sessions" => json!({ "cmd": "list" }),
                "send_message" => json!({
                    "cmd": "send",
                    "args": { "to": str_arg("to")?, "body": str_arg("body")? },
                }),
                "broadcast_message" => json!({
                    "cmd": "broadcast",
                    "args": { "body": str_arg("body")? },
                }),
                "check_inbox" => json!({
                    "cmd": "inbox",
                    "args": { "peek": args.get("peek").and_then(Value::as_bool).unwrap_or(false) },
                }),
                "wait_for_message" => {
                    let mut a = json!({ "wait": true });
                    if let Some(t) = args.get("timeout_seconds").and_then(Value::as_u64) {
                        a["timeout_secs"] = json!(t);
                    }
                    json!({ "cmd": "inbox", "args": a })
                }
                other => return Err(format!("unknown tool: {other}")),
            };
            super::call(req)
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn initialize_handshake() {
                let resp = handle(&json!({
                    "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": { "protocolVersion": "2025-06-18" }
                }))
                .unwrap();
                assert_eq!(resp["result"]["protocolVersion"], "2025-06-18");
                assert_eq!(resp["result"]["serverInfo"]["name"], "termhub-msg");
                assert!(resp["result"]["capabilities"]["tools"].is_object());
            }

            #[test]
            fn notification_gets_no_reply() {
                assert!(handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
                    .is_none());
            }

            #[test]
            fn tools_list_advertises_every_tool() {
                let resp = handle(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
                    .unwrap();
                let names: Vec<&str> = resp["result"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| t["name"].as_str().unwrap())
                    .collect();
                assert_eq!(
                    names,
                    [
                        "list_sessions",
                        "send_message",
                        "broadcast_message",
                        "check_inbox",
                        "wait_for_message"
                    ]
                );
            }

            #[test]
            fn unknown_method_is_a_jsonrpc_error() {
                let resp =
                    handle(&json!({ "jsonrpc": "2.0", "id": 3, "method": "does/notexist" })).unwrap();
                assert_eq!(resp["error"]["code"], -32601);
            }

            #[test]
            fn tools_call_missing_arg_is_reported_in_band() {
                // No socket, but the missing-argument check fires before any connect attempt.
                let resp = handle(&json!({
                    "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                    "params": { "name": "send_message", "arguments": { "to": "b" } }
                }))
                .unwrap();
                assert_eq!(resp["result"]["isError"], true);
                assert!(resp["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("body"));
            }
        }

        fn tool_schemas() -> Value {
            let str_prop = |desc: &str| json!({ "type": "string", "description": desc });
            json!([
                {
                    "name": "list_sessions",
                    "description": "List the other open TermHub sessions (id, name, cwd, is_current, unread count).",
                    "inputSchema": { "type": "object", "properties": {} },
                },
                {
                    "name": "send_message",
                    "description": "Send a message to one other session, by name (case-insensitive) or id.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "to": str_prop("target session name or id"),
                            "body": str_prop("message text"),
                        },
                        "required": ["to", "body"],
                    },
                },
                {
                    "name": "broadcast_message",
                    "description": "Send a message to every other open session at once.",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "body": str_prop("message text") },
                        "required": ["body"],
                    },
                },
                {
                    "name": "check_inbox",
                    "description": "Read this session's unread messages. Marks them read unless peek is true.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "peek": { "type": "boolean", "description": "leave the messages unread" },
                        },
                    },
                },
                {
                    "name": "wait_for_message",
                    "description": "Block until a message arrives for this session, or the timeout elapses (returns an empty list on timeout).",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "timeout_seconds": {
                                "type": "integer",
                                "description": "max seconds to wait (default 60, max 600)",
                            },
                        },
                    },
                },
            ])
        }
    }
}
