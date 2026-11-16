//! Tauri commands for editing (M6). Every edit: validate through `editing::apply`, update the
//! open show, tell the engine (`UpdatePlan`: playback carries on), schedule an autosave.

use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Map, Value};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::AppState;
use crate::commands::{ShowView, view};
use crate::editing::{self, Edit};
use crate::saver::{SaveState, save_now};

fn busy() -> String {
    "ThornCue is busy. Try again.".into()
}

#[derive(Debug, Clone, Serialize)]
pub struct EditResult {
    pub show: ShowView,
    /// The cue to inspect now (a new cue, or the one after a deleted one).
    pub focus: Option<String>,
    /// Things that didn't work (files that aren't audio), as sentences.
    pub problems: Vec<String>,
}

fn edit(
    app: &AppHandle,
    show_id: Option<&str>,
    e: Edit,
    problems: Vec<String>,
) -> Result<EditResult, String> {
    let state = app.state::<AppState>();
    let mut guard = state.open_show.lock().map_err(|_| busy())?;
    let s = guard.as_mut().ok_or("No show is open.")?;
    if show_id.is_some_and(|id| id != s.id) {
        return Err("The show changed while the files were being copied. Add them again.".into());
    }
    let (m, focus) = editing::apply(&s.manifest, e)?;
    s.manifest = m;
    s.dirty = true;
    let plan = crate::playback::plan_for(&s.dir, &s.manifest, 0);
    let show = view(s);
    drop(guard);
    state
        .engine
        .send(engine::Command::UpdatePlan(plan))
        .map_err(|e| e.to_string())?;
    state.saver.ping();
    let _ = app.emit(
        "show://save",
        SaveState {
            state: "unsaved",
            message: None,
        },
    );
    Ok(EditResult {
        show,
        focus,
        problems,
    })
}

fn open_dir(state: &AppState) -> Result<(String, PathBuf), String> {
    let g = state.open_show.lock().map_err(|_| busy())?;
    let s = g.as_ref().ok_or("No show is open.")?;
    Ok((s.id.clone(), s.dir.clone()))
}

#[tauri::command]
pub fn update_cue(
    id: String,
    patch: Map<String, Value>,
    app: AppHandle,
) -> Result<EditResult, String> {
    edit(&app, None, Edit::Update { id, patch }, vec![])
}

#[tauri::command]
pub fn move_cue(id: String, to: usize, app: AppHandle) -> Result<EditResult, String> {
    edit(&app, None, Edit::Move { id, to }, vec![])
}

#[tauri::command]
pub fn add_control_cue(silence: bool, app: AppHandle) -> Result<EditResult, String> {
    edit(&app, None, Edit::AddControl { silence }, vec![])
}

#[tauri::command]
pub fn duplicate_cue(id: String, app: AppHandle) -> Result<EditResult, String> {
    edit(&app, None, Edit::Duplicate { id }, vec![])
}

#[tauri::command]
pub fn delete_cue(id: String, app: AppHandle) -> Result<EditResult, String> {
    edit(&app, None, Edit::Delete { id }, vec![])
}

#[tauri::command]
pub fn set_show_name(name: String, app: AppHandle) -> Result<EditResult, String> {
    edit(&app, None, Edit::ShowName(name), vec![])
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| {
        tracing::error!("edit task failed: {e}");
        "That stopped unexpectedly. Try again.".to_string()
    })?
}

/// Add audio cues: each file is checked and copied into the show folder (off the main
/// thread), then the cues are added at the end. Files that aren't audio are reported.
#[tauri::command]
pub async fn add_audio_cues(paths: Vec<String>, app: AppHandle) -> Result<EditResult, String> {
    blocking(move || {
        let (id, dir) = open_dir(&app.state::<AppState>())?;
        let mut files = Vec::new();
        let mut problems = Vec::new();
        for p in paths {
            match editing::copy_audio_in(&dir, &PathBuf::from(&p)) {
                Ok(f) => files.push(f),
                Err(e) => problems.push(e),
            }
        }
        if files.is_empty() {
            return Err(problems.join(" "));
        }
        edit(&app, Some(&id), Edit::AddAudio(files), problems)
    })
    .await
}

#[tauri::command]
pub async fn replace_cue_file(
    id: String,
    path: String,
    app: AppHandle,
) -> Result<EditResult, String> {
    blocking(move || {
        let (show, dir) = open_dir(&app.state::<AppState>())?;
        let audio = editing::copy_audio_in(&dir, &PathBuf::from(&path))?;
        edit(&app, Some(&show), Edit::ReplaceAudio { id, audio }, vec![])
    })
    .await
}

// ---------------------------------------------------------------------------
// Library: new, duplicate, export
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn new_show(state: State<'_, AppState>) -> Result<String, String> {
    state.library.create_show("Untitled show").map_err(|e| {
        tracing::error!("creating a show: {e:#}");
        "Couldn't create the show. Check there is free disk space.".to_string()
    })
}

#[tauri::command]
pub async fn duplicate_show(id: String, app: AppHandle) -> Result<String, String> {
    blocking(move || {
        save_now(&app)?;
        app.state::<AppState>()
            .library
            .duplicate_show(&id)
            .map_err(|e| {
                tracing::error!("duplicating {id}: {e:#}");
                "Couldn't duplicate the show. Check there is free disk space.".to_string()
            })
    })
    .await
}

/// The file name the save dialog suggests: the show name made safe, plus .tqs (FORMAT.md 3).
#[tauri::command]
pub fn export_file_name(id: String, state: State<'_, AppState>) -> Result<String, String> {
    let s = state
        .library
        .list()
        .map_err(|_| busy())?
        .into_iter()
        .find(|s| s.id == id)
        .ok_or("No such show.")?;
    Ok(format!("{}.tqs", crate::library::safe_file_name(&s.name)))
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportProgress {
    pub done: u64,
    pub total: u64,
}

/// Write the show as a `.tqs` at `path`, saving any pending edits first.
#[tauri::command]
pub async fn export_show(id: String, path: String, app: AppHandle) -> Result<(), String> {
    blocking(move || {
        save_now(&app)?;
        let state = app.state::<AppState>();
        let dir = state
            .library
            .show_dir(&id)
            .map_err(|_| "No such show.".to_string())?;
        // A damaged manifest is recovered from its backup (and repaired) first.
        let recovered = state.library.ensure_readable(&id).map_err(|e| {
            tracing::error!("exporting {id}: {e:#}");
            "This show's files are damaged and there is no good backup, so it can't be exported."
                .to_string()
        })?;
        if recovered.is_some() {
            tracing::warn!("exporting {id} from its backup");
        }
        let mut last = std::time::Instant::now();
        let mut on_progress = |p: tqs::Progress| {
            if last.elapsed().as_millis() >= 50 || p.done == p.total {
                last = std::time::Instant::now();
                let _ = app.emit(
                    "export://progress",
                    ExportProgress {
                        done: p.done,
                        total: p.total,
                    },
                );
            }
        };
        tqs::write_package_with_progress(&dir, &PathBuf::from(&path), &mut on_progress).map_err(
            |e| {
                tracing::error!("exporting {id}: {e}");
                e.to_string()
            },
        )?;
        tracing::info!("exported {id} to {path}");
        Ok(())
    })
    .await
}

// ---------------------------------------------------------------------------
// Trim editor (M7): peaks and audition
// ---------------------------------------------------------------------------

/// The open show's audio file for cue `id`.
fn cue_audio_path(state: &AppState, id: &str) -> Result<(usize, PathBuf), String> {
    let g = state.open_show.lock().map_err(|_| busy())?;
    let s = g.as_ref().ok_or("No show is open.")?;
    let (index, cue) = s
        .manifest
        .cues
        .iter()
        .enumerate()
        .find(|(_, c)| c.id == id)
        .ok_or("That cue is no longer in the show.")?;
    let asset = cue
        .asset
        .as_ref()
        .and_then(|a| s.manifest.assets.get(a))
        .ok_or("This cue has no audio file.")?;
    Ok((index, s.dir.join(&asset.path)))
}

/// Waveform peaks for cue `id`'s file (the whole file; trims and cuts are drawn over it).
#[tauri::command]
pub async fn get_peaks(id: String, app: AppHandle) -> Result<engine::Peaks, String> {
    blocking(move || {
        let state = app.state::<AppState>();
        let (_, path) = cue_audio_path(&state, &id)?;
        let (tx, rx) = crossbeam_channel::bounded(1);
        state
            .engine
            .send(engine::Command::Peaks { path, reply: tx })
            .map_err(|e| e.to_string())?;
        // A long file may still be decoding; it answers as soon as it has.
        rx.recv_timeout(std::time::Duration::from_secs(120))
            .map_err(|_| {
                "The waveform took too long to load. Try selecting the cue again.".to_string()
            })?
    })
    .await
}

/// Play cue `id` from `from` seconds into its file (the trim editor's "Audition from marker").
#[tauri::command]
pub fn audition(id: String, from: f64, state: State<'_, AppState>) -> Result<(), String> {
    if !from.is_finite() {
        return Err("That isn't a position in the file.".into());
    }
    let (index, _) = cue_audio_path(&state, &id)?;
    state
        .engine
        .send(engine::Command::Audition { index, from })
        .map_err(|e| e.to_string())
}

/// The trim editor's gain line (thorncue-spec#5), from playback's own fade envelope.
#[tauri::command]
pub fn fade_curve(
    shape: engine::FadeShape,
    duration: f64,
    columns: usize,
) -> Result<Vec<Option<f32>>, String> {
    if !duration.is_finite() || duration <= 0.0 {
        return Ok(Vec::new());
    }
    Ok(engine::fade_curve(&shape, duration, columns.min(16_384)))
}

#[tauri::command]
pub fn stop_audition(state: State<'_, AppState>) -> Result<(), String> {
    state
        .engine
        .send(engine::Command::StopAudition)
        .map_err(|e| e.to_string())
}
