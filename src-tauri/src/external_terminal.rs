use std::process::Command;

/// (display name, bundle identifier) for terminal apps we know how to launch. Matching by
/// bundle ID rather than guessing `/Applications/<name>.app` avoids false negatives from
/// app-name/filename mismatches (e.g. iTerm2 ships as "iTerm 2.app", not "iTerm.app") and
/// from apps living outside the two conventional install directories.
#[cfg(target_os = "macos")]
const KNOWN_APPS: &[(&str, &str)] = &[
    ("Terminal", "com.apple.Terminal"),
    ("iTerm", "com.googlecode.iterm2"),
    ("Warp", "dev.warp.Warp-Stable"),
    ("Alacritty", "org.alacritty"),
    ("WezTerm", "com.github.wez.wezterm"),
    ("Hyper", "co.zeit.hyper"),
    ("kitty", "net.kovidgoyal.kitty"),
];

#[cfg(target_os = "macos")]
fn bundle_id_for(app: &str) -> Option<&'static str> {
    KNOWN_APPS
        .iter()
        .find(|(name, _)| *name == app)
        .map(|(_, id)| *id)
}

#[cfg(target_os = "macos")]
fn is_installed(bundle_id: &str) -> bool {
    Command::new("mdfind")
        .arg(format!("kMDItemCFBundleIdentifier == '{bundle_id}'"))
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
pub fn list_apps() -> Vec<String> {
    KNOWN_APPS
        .iter()
        .filter(|(_, bundle_id)| is_installed(bundle_id))
        .map(|(name, _)| name.to_string())
        .collect()
}

#[cfg(target_os = "windows")]
pub fn list_apps() -> Vec<String> {
    let mut apps = vec!["Command Prompt".to_string(), "PowerShell".to_string()];
    let has_wt = Command::new("where")
        .arg("wt.exe")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if has_wt {
        apps.insert(0, "Windows Terminal".to_string());
    }
    apps
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn list_apps() -> Vec<String> {
    vec![]
}

#[cfg(target_os = "macos")]
pub fn open_external(app: &str, cwd: &str) -> Result<(), String> {
    let bundle_id = bundle_id_for(app).ok_or_else(|| format!("Unknown terminal app: {app}"))?;
    Command::new("open")
        .args(["-b", bundle_id, cwd])
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn open_external(app: &str, cwd: &str) -> Result<(), String> {
    let result = match app {
        "Windows Terminal" => Command::new("wt.exe").args(["-d", cwd]).spawn(),
        "PowerShell" => Command::new("cmd")
            .args(["/C", "start", "powershell", "-NoExit", "-Command", &format!("cd '{cwd}'")])
            .spawn(),
        _ => Command::new("cmd")
            .args(["/C", "start", "cmd", "/K", &format!("cd /d {cwd}")])
            .spawn(),
    };
    result.map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn open_external(_app: &str, _cwd: &str) -> Result<(), String> {
    Err("Opening an external terminal is not supported on this platform".into())
}
