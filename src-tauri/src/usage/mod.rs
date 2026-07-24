mod aider;
mod claude_code;
mod codex;
mod gemini;
mod tracker;

pub use tracker::spawn_tracker;

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct UsageEvent {
    pub cwd: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
    /// Unix seconds.
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionUsage {
    pub session_id: Option<String>,
    pub session_name: String,
    pub agent: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentUsage {
    pub agent: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DayUsage {
    pub day: String,
    pub agent: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageSummary {
    pub per_session: Vec<SessionUsage>,
    pub per_agent: Vec<AgentUsage>,
    pub per_day: Vec<DayUsage>,
    pub total_tokens_in: i64,
    pub total_tokens_out: i64,
}

/// A source of local agent-CLI usage data (a log/transcript format on disk). Adapters are
/// pluggable so more agents can be added without touching the tracker or the DB schema —
/// each just needs to say which files to watch and how to turn new bytes into `UsageEvent`s.
pub trait UsageAdapter: Send + Sync {
    fn agent_name(&self) -> &'static str;

    /// All log files currently on disk for this agent. `known_cwds` is every cwd TermHub has
    /// ever opened a session in — agents that log per-project (e.g. Aider, straight into the
    /// project folder) use it instead of a global log directory; agents with their own global
    /// log dir (Claude Code, Codex) just ignore it.
    fn discover_files(&self, known_cwds: &[String]) -> Vec<PathBuf>;

    /// Parse events found after `since_offset` bytes into the file. Returns the events and the
    /// new byte offset to resume from on the next poll.
    fn parse_new_events(
        &self,
        path: &Path,
        since_offset: u64,
    ) -> Result<(Vec<UsageEvent>, u64), String>;
}

/// Opens `path`, seeks to `since_offset` (or 0 if the file's shrunk since then — rotated or
/// truncated), and returns every *complete* (newline-terminated) line found since, plus the
/// byte offset to resume from next time. Deliberately does **not** return a trailing line with
/// no `\n` yet: `tracker.rs` polls every few seconds while the owning agent CLI can still be
/// mid-write to the file, and every adapter here used to advance its offset straight to
/// end-of-file regardless — a line caught half-written failed to parse (or parsed as garbage)
/// and its tokens were lost for good, since the offset had already moved past it and nothing
/// ever re-read it. Left unconsumed here, that same partial line is simply read again, complete,
/// on the next poll.
fn read_new_lines(path: &Path, since_offset: u64) -> Result<(Vec<String>, u64), String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let start = if len < since_offset { 0 } else { since_offset };
    file.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(|e| e.to_string())?;

    let Some(last_newline) = buf.iter().rposition(|&b| b == b'\n') else {
        // Not even one complete line since `start` — nothing to parse yet, and don't advance
        // past it so the next poll re-reads it once it actually has a terminator.
        return Ok((Vec::new(), start));
    };
    let lines = String::from_utf8_lossy(&buf[..=last_newline])
        .lines()
        .map(str::to_string)
        .collect();
    Ok((lines, start + last_newline as u64 + 1))
}

pub fn all_adapters() -> Vec<Box<dyn UsageAdapter>> {
    vec![
        Box::new(claude_code::ClaudeCodeAdapter),
        Box::new(codex::CodexAdapter),
        Box::new(aider::AiderAdapter),
        Box::new(gemini::GeminiAdapter),
    ]
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

/// Minimal RFC3339 -> unix-seconds parser (no chrono/time dependency). Handles the
/// "YYYY-MM-DDTHH:MM:SS(.fff)?Z" shape both Claude Code's and Codex's logs use.
fn parse_rfc3339_to_unix(s: &str) -> Option<i64> {
    let s = s.trim_end_matches('Z');
    let (date_part, time_part) = s.split_once('T')?;
    parse_date_time_parts(date_part, time_part)
}

/// Aider's "# aider chat started at YYYY-MM-DD HH:MM:SS" timestamp — space-separated, and in
/// the local wall-clock time it was written in rather than UTC. We treat it as UTC anyway
/// (same simplification everywhere here, no timezone lookup) — good enough for day-level
/// bucketing, occasionally off by a day right at a local midnight boundary.
fn parse_local_datetime_to_unix(s: &str) -> Option<i64> {
    let (date_part, time_part) = s.trim().split_once(' ')?;
    parse_date_time_parts(date_part, time_part)
}

fn parse_date_time_parts(date_part: &str, time_part: &str) -> Option<i64> {
    let mut date_iter = date_part.split('-');
    let year: i64 = date_iter.next()?.parse().ok()?;
    let month: i64 = date_iter.next()?.parse().ok()?;
    let day: i64 = date_iter.next()?.parse().ok()?;

    let time_main = time_part.split('.').next().unwrap_or(time_part);
    let mut time_iter = time_main.split(':');
    let hour: i64 = time_iter.next()?.parse().ok()?;
    let minute: i64 = time_iter.next()?.parse().ok()?;
    let second: i64 = time_iter.next()?.parse().ok()?;

    let days = days_from_civil(year, month, day);
    Some(days * 86400 + hour * 3600 + minute * 60 + second)
}

/// Parses Aider's compact token counts: "36", "1.9k", "4.2m".
fn parse_compact_token_count(s: &str) -> Option<i64> {
    let s = s.trim();
    let (num_part, multiplier) = if let Some(stripped) = s.strip_suffix(['k', 'K']) {
        (stripped, 1_000.0)
    } else if let Some(stripped) = s.strip_suffix(['m', 'M']) {
        (stripped, 1_000_000.0)
    } else {
        (s, 1.0)
    };
    let value: f64 = num_part.parse().ok()?;
    Some((value * multiplier).round() as i64)
}

/// Howard Hinnant's days-from-civil algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
