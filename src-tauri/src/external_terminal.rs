use std::path::Path;
use std::process::Command;

#[cfg(target_os = "macos")]
pub fn list_apps() -> Vec<String> {
    let candidates = [
        "Terminal", "iTerm", "Warp", "Alacritty", "WezTerm", "Hyper", "kitty",
    ];
    let search_dirs = ["/Applications", "/System/Applications"];
    candidates
        .iter()
        .filter(|name| {
            search_dirs
                .iter()
                .any(|dir| Path::new(&format!("{dir}/{name}.app")).exists())
        })
        .map(|s| s.to_string())
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
    Command::new("open")
        .args(["-a", app, cwd])
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
