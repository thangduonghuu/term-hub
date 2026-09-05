use serde::Serialize;

/// One inter-session message row (see `docs/intersession-messaging-plan.md` and `db.rs`'s
/// `messages` table). `from_name` is the sender session's display name at read time, resolved
/// via a `LEFT JOIN sessions` — `None` when the sender was outside any session or its session
/// row is already gone.
#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub id: i64,
    pub from_session: Option<String>,
    pub from_name: Option<String>,
    pub body: String,
    pub created_at: i64,
}

/// One row for the message-log panel (`MessageLog.tsx` via `get_message_log`) — both endpoints
/// by display name, so it reads as a transcript regardless of read/unread state. The session
/// ids ride along too so the panel can map each endpoint to its `#N` and its per-session bubble
/// colour; either is `None` when that side was an outside shell or a since-closed session.
#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub from_id: Option<String>,
    pub from_name: Option<String>,
    pub to_id: Option<String>,
    pub to_name: Option<String>,
    pub body: String,
    pub created_at: i64,
}
