use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub shell: String,
    /// Extra argv for `shell` — empty for an ordinary interactive shell (`tty::Shell::new`'s
    /// second argument). Non-empty for an SSH-backed session (see `commands::connect_ssh_session`),
    /// where `shell` is `"ssh"` and this carries `-i <identity>`/`-p <port>`/`user@host`.
    #[serde(default)]
    pub shell_args: Vec<String>,
    pub created_at: i64,
}

/// Which of `SshCredential`'s two mutually-exclusive auth fields (`password`/`key_id`) is
/// actually used to connect — mirrors the picker's Password/SSH Key toggle in `SshConnect.tsx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SshAuthMethod {
    Password,
    Key,
}

impl SshAuthMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            SshAuthMethod::Password => "password",
            SshAuthMethod::Key => "key",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "password" => Some(SshAuthMethod::Password),
            "key" => Some(SshAuthMethod::Key),
            _ => None,
        }
    }
}

/// A saved VPS login — Termius-style credential the user picks from `SshConnect.tsx` instead of
/// retyping host/user/auth every time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshCredential {
    pub id: String,
    /// User-facing name for the picker, e.g. "Prod API box" — distinct from `username`.
    pub label: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: SshAuthMethod,
    /// Set when `auth_method` is `Password`. Stored as plaintext (same tradeoff this app
    /// already makes for `commands::ANTHROPIC_API_KEY_SETTING`). `ssh` is still spawned as a
    /// plain interactive prompt (see `connect_ssh_session`'s doc comment) — this is typed in
    /// automatically the moment that prompt appears (`App::pending_ssh_passwords` in `lib.rs`),
    /// rather than fed to `ssh` directly, so the real login sequence in the tile looks exactly
    /// like it would if you'd typed it yourself.
    pub password: Option<String>,
    /// Set when `auth_method` is `Key` — id of a `SshKey` in the vault (`ssh_keys` table).
    pub key_id: Option<String>,
    pub created_at: i64,
}

/// A saved private key in TermHub's own vault — added once (pasted or read from a file) and
/// reused by any number of credentials, so nothing ever asks you to re-browse a filesystem path.
/// The `content` field (the actual key bytes) never leaves the Rust side: only `SshKeySummary`
/// (below) is ever serialized back to the frontend.
#[derive(Debug, Clone)]
pub struct SshKey {
    pub id: String,
    pub name: String,
    pub content: String,
    pub created_at: i64,
}

/// `SshKey` minus `content` — everything the "SSH Key" picker in `SshConnect.tsx` is allowed to
/// see. Keeping this a separate type (rather than `#[serde(skip)]` on `SshKey::content`) makes
/// "never send key material to the webview" a compile-time property of which type a command
/// returns, not something that relies on remembering a skip annotation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshKeySummary {
    pub id: String,
    pub name: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    #[serde(flatten)]
    pub meta: SessionMeta,
}

pub fn default_shell() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".into())
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
    }
}

pub fn default_cwd() -> String {
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into())
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOME").unwrap_or_else(|_| "/".into())
    }
}
