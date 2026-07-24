use std::path::{Path, PathBuf};

use super::{home_dir, parse_rfc3339_to_unix, read_new_lines, UsageAdapter, UsageEvent};

/// Reads Claude Code's per-project session transcripts at
/// `~/.claude/projects/<cwd-with-dashes>/<sessionId>.jsonl`. Each line is a JSON event; token
/// counts live on `type: "assistant"` lines under `message.usage`.
pub struct ClaudeCodeAdapter;

impl UsageAdapter for ClaudeCodeAdapter {
    fn agent_name(&self) -> &'static str {
        "claude-code"
    }

    fn discover_files(&self, _known_cwds: &[String]) -> Vec<PathBuf> {
        let Some(home) = home_dir() else {
            return vec![];
        };
        let projects_dir = home.join(".claude").join("projects");
        let mut files = Vec::new();
        let Ok(project_entries) = std::fs::read_dir(&projects_dir) else {
            return files;
        };
        for project_entry in project_entries.flatten() {
            let project_path = project_entry.path();
            if !project_path.is_dir() {
                continue;
            }
            let Ok(session_entries) = std::fs::read_dir(&project_path) else {
                continue;
            };
            for session_entry in session_entries.flatten() {
                let path = session_entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    files.push(path);
                }
            }
        }
        files
    }

    fn parse_new_events(
        &self,
        path: &Path,
        since_offset: u64,
    ) -> Result<(Vec<UsageEvent>, u64), String> {
        let (lines, new_offset) = read_new_lines(path, since_offset)?;
        let mut events = Vec::new();
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            if let Some(event) = parse_line(&line) {
                events.push(event);
            }
        }
        Ok((events, new_offset))
    }
}

fn parse_line(line: &str) -> Option<UsageEvent> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let usage = value.get("message")?.get("usage")?;
    let input_tokens = usage.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
    let cache_creation = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let output_tokens = usage.get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
    let cwd = value.get("cwd")?.as_str()?.to_string();
    let timestamp = value
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(parse_rfc3339_to_unix)
        .unwrap_or(0);

    Some(UsageEvent {
        cwd,
        tokens_in: input_tokens + cache_creation + cache_read,
        tokens_out: output_tokens,
        timestamp,
    })
}
