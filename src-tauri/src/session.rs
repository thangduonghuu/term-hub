use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub shell: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    #[serde(flatten)]
    pub meta: SessionMeta,
    pub running: bool,
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
