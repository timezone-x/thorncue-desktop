//! Show readiness (M8, thorncue-spec#8): keep the computer awake while a show is open, and
//! after an unclean exit reopen the last show at its standby cue.
//!
//! Crash recovery uses a marker, `<app_data_dir>/session.json`, written (atomically) when a show
//! opens and removed when it closes or the app exits cleanly. If it is still there at the next
//! start, the last run didn't end cleanly (crash, kill, power cut), and the show it names is
//! opened again. Standby comes from that show's `local.json`, which the standby saver keeps
//! current (playback.rs). Nothing plays: the operator presses GO.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Marker {
    show: String,
}

pub struct Session {
    path: PathBuf,
    /// The show the last run had open when it ended uncleanly; taken once.
    recovered: Mutex<Option<String>>,
    awake: Mutex<Awake>,
}

#[derive(Default)]
struct Awake {
    guard: Option<keepawake::KeepAwake>,
    /// The operator has been told keep-awake isn't working on this machine (once per run).
    warned: bool,
}

impl Session {
    /// Read (and clear) the marker the last run left behind, if any.
    pub fn start(data_dir: &Path) -> Self {
        let path = data_dir.join("session.json");
        let recovered = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Marker>(&b).ok())
            .map(|m| m.show);
        if let Some(id) = &recovered {
            tracing::warn!("the last run didn't close cleanly; it had show {id} open");
        }
        let _ = std::fs::remove_file(&path);
        Self {
            path,
            recovered: Mutex::new(recovered),
            awake: Mutex::new(Awake::default()),
        }
    }

    /// The show to reopen after an unclean exit. Only the first call gets it.
    pub fn take_recovered(&self) -> Option<String> {
        self.recovered.lock().ok()?.take()
    }

    /// A show is open: remember it, and keep the computer awake. Returns a sentence for the
    /// operator the first time keep-awake can't be had on this machine.
    pub fn show_opened(&self, show_id: &str) -> Option<String> {
        let marker = Marker {
            show: show_id.to_string(),
        };
        if let Err(e) = crate::fsutil::atomic_write(
            &self.path,
            &serde_json::to_vec(&marker).expect("a marker serialises"),
        ) {
            tracing::warn!("couldn't write the session marker: {e:#}");
        }
        // Once a show is open, anything left to recover is out of date.
        if let Ok(mut r) = self.recovered.lock() {
            *r = None;
        }
        self.stay_awake()
    }

    /// The show was closed (or the app is quitting cleanly).
    pub fn show_closed(&self) {
        let _ = std::fs::remove_file(&self.path);
        if let Ok(mut a) = self.awake.lock() {
            a.guard = None;
        }
    }

    fn stay_awake(&self) -> Option<String> {
        let mut a = self.awake.lock().ok()?;
        if a.guard.is_some() {
            return None;
        }
        match keepawake::Builder::default()
            .display(true)
            .idle(true)
            .reason("A show is open")
            .app_name("ThornCue")
            .app_reverse_domain("xyz.thorngate.thorncue")
            .create()
        {
            Ok(g) => {
                tracing::info!("keeping the computer awake while the show is open");
                a.guard = Some(g);
                None
            }
            Err(e) => {
                tracing::warn!("keep-awake isn't available: {e:#}");
                if a.warned {
                    return None;
                }
                a.warned = true;
                Some(
                    "ThornCue couldn't stop this computer sleeping. Turn off sleep and the screen \
                     saver in the system settings before the show."
                        .into(),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_run_leaves_nothing_to_recover() {
        let dir = tempfile::tempdir().unwrap();
        let s = Session::start(dir.path());
        let _ = s.show_opened("abc123");
        assert!(dir.path().join("session.json").is_file());
        s.show_closed();
        drop(s);
        assert_eq!(Session::start(dir.path()).take_recovered(), None);
    }

    #[test]
    fn an_unclean_exit_reopens_the_show_once() {
        let dir = tempfile::tempdir().unwrap();
        let s = Session::start(dir.path());
        let _ = s.show_opened("abc123");
        // The process dies here: show_closed never runs.
        drop(s);
        let next = Session::start(dir.path());
        assert!(
            !dir.path().join("session.json").exists(),
            "read and cleared at start"
        );
        assert_eq!(next.take_recovered().as_deref(), Some("abc123"));
        assert_eq!(next.take_recovered(), None, "only once");
        // And a crash on that next run, before any show opens, has nothing to recover.
        drop(next);
        assert_eq!(Session::start(dir.path()).take_recovered(), None);
    }

    #[test]
    fn opening_another_show_replaces_what_would_be_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let s = Session::start(dir.path());
        let _ = s.show_opened("first");
        drop(s);
        let s = Session::start(dir.path());
        let _ = s.show_opened("second");
        assert_eq!(
            s.take_recovered(),
            None,
            "the operator already opened a show"
        );
        drop(s);
        assert_eq!(
            Session::start(dir.path()).take_recovered().as_deref(),
            Some("second")
        );
    }

    #[test]
    fn a_damaged_marker_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("session.json"), b"{not json").unwrap();
        assert_eq!(Session::start(dir.path()).take_recovered(), None);
        assert!(!dir.path().join("session.json").exists());
    }
}
