use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

use super::{parse_compact_token_count, parse_local_datetime_to_unix, UsageAdapter, UsageEvent};

/// Aider logs straight into the project it's run from — `<cwd>/.aider.chat.history.md` — rather
/// than a global log directory, so this only looks inside cwds TermHub already knows about
/// (every event found this way is guaranteed to match a TermHub session by cwd, unlike the
/// other adapters' best-effort match).
pub struct AiderAdapter;

const HISTORY_FILENAME: &str = ".aider.chat.history.md";

impl UsageAdapter for AiderAdapter {
    fn agent_name(&self) -> &'static str {
        "aider"
    }

    fn discover_files(&self, known_cwds: &[String]) -> Vec<PathBuf> {
        known_cwds
            .iter()
            .map(|cwd| PathBuf::from(cwd).join(HISTORY_FILENAME))
            .filter(|p| p.is_file())
            .collect()
    }

    fn parse_new_events(
        &self,
        path: &PathBuf,
        since_offset: u64,
    ) -> Result<(Vec<UsageEvent>, u64), String> {
        let cwd = path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        let start = if len < since_offset { 0 } else { since_offset };
        file.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
        let reader = BufReader::new(file);

        // "Tokens: X sent, Y received." lines don't carry their own timestamp — only each
        // session's "# aider chat started at ..." header does — so track the most recent one.
        let mut current_timestamp = 0i64;
        let mut events = Vec::new();

        for line in reader.lines() {
            let line = line.map_err(|e| e.to_string())?;
            let trimmed = line.trim();

            if let Some(rest) = trimmed.strip_prefix("# aider chat started at ") {
                if let Some(ts) = parse_local_datetime_to_unix(rest) {
                    current_timestamp = ts;
                }
                continue;
            }

            let Some(rest) = trimmed
                .strip_prefix("> Tokens: ")
                .map(|s| s.trim_end_matches('.'))
            else {
                continue;
            };
            let Some((sent_part, received_part)) = rest.split_once(", ") else {
                continue;
            };
            let Some(sent_str) = sent_part.strip_suffix(" sent") else {
                continue;
            };
            let Some(received_str) = received_part.strip_suffix(" received") else {
                continue;
            };
            let Some(tokens_in) = parse_compact_token_count(sent_str) else {
                continue;
            };
            let Some(tokens_out) = parse_compact_token_count(received_str) else {
                continue;
            };

            events.push(UsageEvent {
                cwd: cwd.clone(),
                tokens_in,
                tokens_out,
                timestamp: current_timestamp,
            });
        }

        Ok((events, len))
    }
}
