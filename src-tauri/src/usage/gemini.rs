use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::{home_dir, parse_rfc3339_to_unix, UsageAdapter, UsageEvent};

/// Reads Gemini CLI's per-project session logs at `~/.gemini/tmp/<project-hash>/chats/*.jsonl`.
pub struct GeminiAdapter;

impl UsageAdapter for GeminiAdapter {
    fn agent_name(&self) -> &'static str {
        "gemini"
    }

    fn discover_files(&self, _known_cwds: &[String]) -> Vec<PathBuf> {
        let Some(home) = home_dir() else {
            return vec![];
        };
        let tmp_dir = home.join(".gemini").join("tmp");
        let mut files = Vec::new();
        let Ok(project_entries) = std::fs::read_dir(&tmp_dir) else {
            return files;
        };
        for project_entry in project_entries.flatten() {
            let chats_dir = project_entry.path().join("chats");
            let Ok(chat_entries) = std::fs::read_dir(&chats_dir) else {
                continue;
            };
            for chat_entry in chat_entries.flatten() {
                let path = chat_entry.path();
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
        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        let start = if len < since_offset { 0 } else { since_offset };
        file.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
        let reader = BufReader::new(file);

        let mut events = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|e| e.to_string())?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(event) = parse_line(&line) {
                events.push(event);
            }
        }
        Ok((events, len))
    }
}

fn parse_line(line: &str) -> Option<UsageEvent> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let usage = find_key(&value, "usageMetadata")?;
    let prompt_tokens = usage
        .get("promptTokenCount")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let candidates_tokens = usage
        .get("candidatesTokenCount")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if prompt_tokens == 0 && candidates_tokens == 0 {
        return None;
    }

    let timestamp = find_timestamp(&value).unwrap_or(0);
    let cwd = find_string(&value, &["cwd", "projectRoot", "workspaceRoot"]).unwrap_or_default();

    Some(UsageEvent {
        cwd,
        tokens_in: prompt_tokens,
        tokens_out: candidates_tokens,
        timestamp,
    })
}

/// Depth-first search for an object value under `key`, since the exact nesting per line isn't
/// confirmed against a real sample.
fn find_key<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    let obj = value.as_object()?;
    if let Some(v) = obj.get(key) {
        return Some(v);
    }
    for v in obj.values() {
        if let Some(found) = find_key(v, key) {
            return Some(found);
        }
    }
    None
}

fn find_timestamp(value: &serde_json::Value) -> Option<i64> {
    let obj = value.as_object()?;
    for key in ["timestamp", "createdAt", "time"] {
        if let Some(v) = obj.get(key) {
            if let Some(s) = v.as_str().and_then(parse_rfc3339_to_unix) {
                return Some(s);
            }
            if let Some(n) = v.as_i64() {
                return Some(n);
            }
        }
    }
    for v in obj.values() {
        if let Some(found) = find_timestamp(v) {
            return Some(found);
        }
    }
    None
}

fn find_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    let obj = value.as_object()?;
    for key in keys {
        if let Some(s) = obj.get(*key).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    for v in obj.values() {
        if let Some(found) = find_string(v, keys) {
            return Some(found);
        }
    }
    None
}
