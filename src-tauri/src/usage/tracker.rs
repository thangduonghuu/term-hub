use std::sync::Arc;
use std::time::Duration;

use super::{all_adapters, UsageAdapter};
use crate::db::Db;

const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Runs for the lifetime of the app, tailing every adapter's log files and writing new usage
/// events into SQLite. Polling (rather than a filesystem watcher) keeps this simple and copes
/// fine with jsonl files that get appended to in bursts.
pub fn spawn_tracker(db: Arc<Db>) {
    std::thread::spawn(move || {
        let adapters = all_adapters();
        loop {
            for adapter in &adapters {
                poll_adapter(adapter.as_ref(), &db);
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    });
}

fn poll_adapter(adapter: &dyn UsageAdapter, db: &Db) {
    for path in adapter.discover_files() {
        let path_str = path.to_string_lossy().to_string();
        let offset = db.get_file_offset(&path_str).unwrap_or(0);
        let Ok((events, new_offset)) = adapter.parse_new_events(&path, offset) else {
            continue; // transient read error; retry next poll
        };
        for event in &events {
            let session_id = db.find_session_id_for_cwd(&event.cwd).unwrap_or(None);
            let _ = db.insert_usage_event(
                session_id.as_deref(),
                adapter.agent_name(),
                event.tokens_in,
                event.tokens_out,
                event.timestamp,
            );
        }
        let _ = db.set_file_offset(&path_str, new_offset);
    }
}
