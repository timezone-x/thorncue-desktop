//! Wiring between the engine and the webview: event forwarding, the show plan, and saving
//! standby. Nothing here runs on the engine thread except the non-blocking `on_event` push.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Sender, unbounded};
use engine::{CueKind, Engine, Event, PlanCue, ShowPlan};
use tauri::{AppHandle, Emitter};
use tqs::{CueType, Manifest};

use crate::library::{read_local, update_local};

/// The open show's folder, so standby changes can be saved to its local.json.
pub type OpenShowDir = Arc<Mutex<Option<PathBuf>>>;

/// Start the engine on the default output and forward its events to the webview.
pub fn start_engine(app: AppHandle, open_dir: OpenShowDir) -> Engine {
    let (tx, rx) = unbounded::<Event>();
    let standby_tx = start_standby_saver(open_dir);

    // Forwarder: the engine thread only pushes into a channel; emitting (which talks to
    // the webview) happens here, so a busy UI can never delay GO.
    std::thread::Builder::new()
        .name("thorncue-events".into())
        .spawn(move || {
            let mut last_standby = None;
            for e in rx {
                match e {
                    Event::State(s) => {
                        if last_standby != Some(s.standby) && s.cue_count > 0 {
                            last_standby = Some(s.standby);
                            let _ = standby_tx.send(s.standby);
                        }
                        let _ = app.emit("engine://state", s);
                    }
                    Event::Positions(p) => {
                        let _ = app.emit("engine://positions", p);
                    }
                    Event::Notice(n) => {
                        let _ = app.emit("engine://notice", n);
                    }
                }
            }
        })
        .expect("spawning the event thread");

    Engine::start(engine::cpal_factory(), engine::kira_loader(), move |e| {
        let _ = tx.send(e);
    })
}

/// Writes the latest standby index to the open show's local.json, off every hot path.
fn start_standby_saver(open_dir: OpenShowDir) -> Sender<usize> {
    let (tx, rx) = unbounded::<usize>();
    std::thread::Builder::new()
        .name("thorncue-standby".into())
        .spawn(move || {
            while let Ok(mut standby) = rx.recv() {
                // Only the latest value matters.
                while let Ok(s) = rx.try_recv() {
                    standby = s;
                }
                let dir = open_dir.lock().ok().and_then(|d| d.clone());
                if let Some(dir) = dir
                    && read_local(&dir).standby_index != standby
                    && let Err(e) = update_local(&dir, |l| l.standby_index = standby)
                {
                    tracing::warn!("saving standby: {e:#}");
                }
            }
        })
        .expect("spawning the standby thread");
    tx
}

/// The engine's copy of a show.
pub fn plan_for(dir: &Path, manifest: &Manifest, standby: usize) -> ShowPlan {
    let cues = manifest
        .cues
        .iter()
        .map(|c| PlanCue {
            id: c.id.clone(),
            name: c.name.clone(),
            kind: match c.cue_type {
                CueType::Audio => CueKind::Audio,
                CueType::Silence => CueKind::Silence,
                CueType::Fadeout => CueKind::Fadeout,
            },
            audio: c
                .asset
                .as_ref()
                .and_then(|a| manifest.assets.get(a))
                .map(|a| dir.join(&a.path)),
            volume: c.volume,
            fade_in: c.fade_in,
            fade_out: c.fade_out,
            pre_wait: c.pre_wait,
            duration: c.duration,
            looping: c.looping,
            continue_after: c.continue_mode == tqs::Continue::AfterEnd,
            trim_start: c.trim_start,
            trim_end: c.trim_end,
            cuts: c.cuts.clone(),
        })
        .collect();
    ShowPlan { cues, standby }
}
