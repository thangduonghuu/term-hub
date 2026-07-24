use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use super::{home_dir, parse_rfc3339_to_unix, read_new_lines, UsageAdapter, UsageEvent};

/// Reads Codex CLI's rollout files at `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. The first
/// line (`session_meta`) carries the cwd for the whole file; per-turn token counts come as
/// `event_msg` lines whose payload has `type: "token_count"`.
pub struct CodexAdapter;

impl UsageAdapter for CodexAdapter {
    fn agent_name(&self) -> &'static str {
        "codex"
    }

    fn discover_files(&self, _known_cwds: &[String]) -> Vec<PathBuf> {
        let Some(home) = home_dir() else {
            return vec![];
        };
        let sessions_dir = home.join(".codex").join("sessions");
        let mut files = Vec::new();
        collect_jsonl_files(&sessions_dir, &mut files, 4);
        files
    }

    fn parse_new_events(
        &self,
        path: &Path,
        since_offset: u64,
    ) -> Result<(Vec<UsageEvent>, u64), String> {
        let (lines, new_offset) = read_new_lines(path, since_offset)?;

        // token_count events don't repeat the session's cwd, only session_meta does — if we're
        // resuming mid-file (since_offset > 0) we won't see that line again this poll, so fetch
        // it once. (On the rare rotation/truncation reset `read_new_lines` handles internally,
        // this redundantly re-reads it from the now-different file's first line — harmless.)
        let mut cwd = if since_offset == 0 {
            None
        } else {
            read_cwd_from_start(path)
        };
        let mut events = Vec::new();

        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            match value.get("type").and_then(|v| v.as_str()) {
                Some("session_meta") => {
                    if let Some(c) = value
                        .get("payload")
                        .and_then(|p| p.get("cwd"))
                        .and_then(|v| v.as_str())
                    {
                        cwd = Some(c.to_string());
                    }
                }
                Some("event_msg") => {
                    let payload = value.get("payload");
                    let is_token_count = payload
                        .and_then(|p| p.get("type"))
                        .and_then(|v| v.as_str())
                        == Some("token_count");
                    if !is_token_count {
                        continue;
                    }
                    let Some(cwd) = cwd.clone() else { continue };
                    let Some(usage) = payload
                        .and_then(|p| p.get("info"))
                        .and_then(|i| i.get("last_token_usage"))
                    else {
                        continue;
                    };
                    // input_tokens/output_tokens already include the cached/reasoning
                    // breakdown fields — they're subtotals, not additional tokens.
                    let input_tokens = usage.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                    let output_tokens = usage.get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                    let timestamp = value
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .and_then(parse_rfc3339_to_unix)
                        .unwrap_or(0);
                    events.push(UsageEvent {
                        cwd,
                        tokens_in: input_tokens,
                        tokens_out: output_tokens,
                        timestamp,
                    });
                }
                _ => {}
            }
        }
        Ok((events, new_offset))
    }
}

fn read_cwd_from_start(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let first_line = reader.lines().next()?.ok()?;
    let value: serde_json::Value = serde_json::from_str(&first_line).ok()?;
    value
        .get("payload")?
        .get("cwd")?
        .as_str()
        .map(|s| s.to_string())
}

fn collect_jsonl_files(dir: &Path, out: &mut Vec<PathBuf>, max_depth: u32) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if max_depth > 0 {
                collect_jsonl_files(&path, out, max_depth - 1);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}
