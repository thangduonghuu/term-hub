use uuid::Uuid;

use crate::db::Db;
use crate::external_terminal;
use crate::session::{default_cwd, default_shell, SessionInfo, SessionMeta};
use crate::usage::UsageSummary;

pub fn get_default_cwd() -> String {
    default_cwd()
}

/// Terminal apps installed on this machine (e.g. iTerm2, Warp) that a session's folder
/// can be opened in as an alternative to the built-in embedded terminal.
pub fn list_terminal_apps() -> Vec<String> {
    external_terminal::list_apps()
}

pub fn open_external_terminal(app: &str, cwd: &str) -> Result<(), String> {
    external_terminal::open_external(app, cwd)
}

pub fn list_sessions(db: &Db) -> Result<Vec<SessionInfo>, String> {
    let metas = db.list_sessions().map_err(|e| e.to_string())?;
    Ok(metas.into_iter().map(|meta| SessionInfo { meta }).collect())
}

pub fn create_session(
    db: &Db,
    name: Option<String>,
    cwd: Option<String>,
) -> Result<SessionInfo, String> {
    let id = Uuid::new_v4().to_string();
    let cwd = cwd.unwrap_or_else(default_cwd);
    let shell = default_shell();
    let name = name.unwrap_or_else(|| "Session".to_string());
    let created_at = unix_now();

    let meta = SessionMeta { id, name, cwd, shell, created_at };
    db.insert_session(&meta).map_err(|e| e.to_string())?;

    Ok(SessionInfo { meta })
}

pub fn rename_session(db: &Db, id: &str, name: &str) -> Result<(), String> {
    db.rename_session(id, name).map_err(|e| e.to_string())
}

/// Removes the session from the saved list.
pub fn close_session(db: &Db, id: &str) -> Result<(), String> {
    db.delete_session(id).map_err(|e| e.to_string())
}

pub fn get_usage_summary(db: &Db) -> Result<UsageSummary, String> {
    let per_session = db.usage_per_session().map_err(|e| e.to_string())?;
    let per_agent = db.usage_per_agent().map_err(|e| e.to_string())?;
    let per_day = db.usage_per_day().map_err(|e| e.to_string())?;
    let (total_tokens_in, total_tokens_out) = db.usage_grand_total().map_err(|e| e.to_string())?;
    Ok(UsageSummary { per_session, per_agent, per_day, total_tokens_in, total_tokens_out })
}

const ANTHROPIC_API_KEY_SETTING: &str = "anthropic_api_key";

pub fn has_anthropic_api_key(db: &Db) -> Result<bool, String> {
    Ok(db.get_setting(ANTHROPIC_API_KEY_SETTING).map_err(|e| e.to_string())?.is_some())
}

pub fn set_anthropic_api_key(db: &Db, key: &str) -> Result<(), String> {
    db.set_setting(ANTHROPIC_API_KEY_SETTING, key).map_err(|e| e.to_string())
}

pub fn clear_anthropic_api_key(db: &Db) -> Result<(), String> {
    db.delete_setting(ANTHROPIC_API_KEY_SETTING).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
pub struct ClaudeLimits {
    /// (header name with the "anthropic-ratelimit-" prefix stripped, value) pairs, in
    /// whatever set Anthropic actually returns — kept as raw pairs rather than a fixed
    /// struct since the exact header set isn't publicly guaranteed to be stable.
    pub limits: Vec<(String, String)>,
}

/// Makes one minimal (~1 output token) real request to Anthropic's Messages API purely to
/// read back its `anthropic-ratelimit-*` response headers. This is the org/API-key rate
/// limit (requests & tokens per minute) — a different quota than Claude Code's Pro/Max
/// 5-hour session limit, which has no public API and isn't available here.
///
/// Blocking (not async): the IPC layer already runs every command on its own background
/// thread (see ipc.rs), so there's no event loop to avoid blocking here.
pub fn check_claude_limits(db: &Db) -> Result<ClaudeLimits, String> {
    let key = db
        .get_setting(ANTHROPIC_API_KEY_SETTING)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "No Anthropic API key configured".to_string())?;

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "model": "claude-3-5-haiku-latest",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .map_err(|e| e.to_string())?;

    let mut limits = Vec::new();
    for (name, value) in resp.headers().iter() {
        let name_str = name.as_str();
        if let Some(stripped) = name_str.strip_prefix("anthropic-ratelimit-") {
            if let Ok(v) = value.to_str() {
                limits.push((stripped.to_string(), v.to_string()));
            }
        }
    }

    if limits.is_empty() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(format!("No rate-limit headers in response (status {status}): {body}"));
    }

    Ok(ClaudeLimits { limits })
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
