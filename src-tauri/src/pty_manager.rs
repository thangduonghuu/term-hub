use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

struct LiveSession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    // The pty stays open (and the entry stays in the map) after the shell exits on its own, so
    // the pane keeps its scrollback instead of vanishing — this flag is what tells "exited" apart
    // from "running" for the status indicator, since map presence alone can't anymore.
    alive: Arc<AtomicBool>,
}

#[derive(Clone, Serialize)]
struct PtyOutputPayload<'a> {
    id: &'a str,
    data: &'a str,
}

#[derive(Clone, Serialize)]
struct PtyExitPayload<'a> {
    id: &'a str,
}

pub struct PtyManager {
    sessions: Mutex<HashMap<String, LiveSession>>,
}

impl PtyManager {
    pub fn new() -> Self {
        PtyManager {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .map(|s| s.alive.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    pub fn spawn(
        &self,
        app: &AppHandle,
        id: String,
        cwd: &str,
        shell: &str,
    ) -> Result<(), String> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;

        let mut cmd = CommandBuilder::new(shell);
        #[cfg(not(windows))]
        {
            // Login + interactive shell so PATH/env from .zprofile/.zshrc (nvm, cargo, etc.) is loaded.
            cmd.arg("-il");
        }
        cmd.cwd(cwd);

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| e.to_string())?;
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

        let alive = Arc::new(AtomicBool::new(true));
        let alive_for_thread = alive.clone();
        let app_handle = app.clone();
        let reader_id = id.clone();
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&buf[..n]).into_owned();
                        let _ = app_handle.emit(
                            "pty-output",
                            PtyOutputPayload {
                                id: &reader_id,
                                data: &data,
                            },
                        );
                    }
                    Err(_) => break,
                }
            }
            alive_for_thread.store(false, Ordering::Relaxed);
            let _ = app_handle.emit("pty-exit", PtyExitPayload { id: &reader_id });
        });

        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(
            id,
            LiveSession {
                master: pair.master,
                writer,
                child,
                alive,
            },
        );

        Ok(())
    }

    pub fn write(&self, id: &str, data: &str) -> Result<(), String> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.get_mut(id).ok_or("session not running")?;
        session
            .writer
            .write_all(data.as_bytes())
            .map_err(|e| e.to_string())?;
        session.writer.flush().map_err(|e| e.to_string())
    }

    pub fn resize(&self, id: &str, rows: u16, cols: u16) -> Result<(), String> {
        let sessions = self.sessions.lock().unwrap();
        let session = sessions.get(id).ok_or("session not running")?;
        session
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())
    }

    pub fn kill(&self, id: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(mut session) = sessions.remove(id) {
            let _ = session.child.kill();
        }
    }
}
