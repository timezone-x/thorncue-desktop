//! Autosave (docs/ARCHITECTURE.md "Files and the library"): the open show is written 300 ms
//! after the last edit, and at once on close, show switch, export and quit. Writes are atomic
//! (temp file, fsync, rename); `manifest.json.bak` was refreshed from the last good file when
//! the show was opened. Saving locks the open show, which the transport never touches, so a
//! save can't delay GO.

use std::time::Duration;

use crossbeam_channel::{RecvTimeoutError, Sender, unbounded};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;
use crate::fsutil::atomic_write;
use crate::library::update_local;

const DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Serialize)]
pub struct SaveState {
    /// "unsaved" | "saved" | "error"
    pub state: &'static str,
    pub message: Option<String>,
}

pub struct Saver {
    tx: Sender<()>,
}

impl Saver {
    pub fn start(app: AppHandle) -> Saver {
        let (tx, rx) = unbounded::<()>();
        std::thread::Builder::new()
            .name("thorncue-autosave".into())
            .spawn(move || {
                while rx.recv().is_ok() {
                    // Wait until edits stop for DEBOUNCE.
                    loop {
                        match rx.recv_timeout(DEBOUNCE) {
                            Ok(()) => continue,
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    let _ = save_now(&app);
                }
            })
            .expect("spawning the autosave thread");
        Saver { tx }
    }

    /// The open show changed: save it soon.
    pub fn ping(&self) {
        let _ = self.tx.send(());
    }
}

/// Write the open show if it has unsaved edits. Safe to call from any thread.
pub fn save_now(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut guard = state
        .open_show
        .lock()
        .map_err(|_| "ThornCue is busy. Try again.".to_string())?;
    let Some(show) = guard.as_mut() else {
        return Ok(());
    };
    if !show.dirty {
        return Ok(());
    }
    let result = atomic_write(
        &show.dir.join("manifest.json"),
        &show.manifest.to_json_pretty(),
    );
    match result {
        Ok(()) => {
            show.dirty = false;
            // A show imported from the web now differs from the web's copy.
            if let Err(e) = update_local(&show.dir, |l| {
                if l.imported_revision.is_some() {
                    l.modified_since_import = true;
                }
            }) {
                tracing::warn!("updating local.json: {e:#}");
            }
            let _ = app.emit(
                "show://save",
                SaveState {
                    state: "saved",
                    message: None,
                },
            );
            Ok(())
        }
        Err(e) => {
            tracing::error!("saving {}: {e:#}", show.dir.display());
            let message =
                "Couldn't save the show. Your changes are kept in memory; check there is free disk space.".to_string();
            let _ = app.emit(
                "show://save",
                SaveState {
                    state: "error",
                    message: Some(message.clone()),
                },
            );
            Err(message)
        }
    }
}
