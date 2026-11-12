//! Tauri commands. Each returns `Result<T, String>` where the string is a sentence for the
//! operator; the detail goes to the log.

use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use engine::Command;

use crate::AppState;
use crate::library::{LocalState, ShowSummary};
use crate::settings::{FAMILIES, MODES, Settings};

fn busy() -> String {
    "ThornCue is busy. Try again.".into()
}

// ---------------------------------------------------------------------------
// Settings and window
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    Ok(state.settings.lock().map_err(|_| busy())?.clone())
}

#[tauri::command]
pub fn set_theme(
    family: String,
    mode: String,
    state: State<'_, AppState>,
) -> Result<Settings, String> {
    if !FAMILIES.contains(&family.as_str()) || !MODES.contains(&mode.as_str()) {
        return Err("That theme isn't one ThornCue knows.".into());
    }
    let mut s = state.settings.lock().map_err(|_| busy())?;
    s.theme_family = family;
    s.theme_mode = mode;
    s.save(&state.settings_path).map_err(|e| {
        tracing::error!("saving settings: {e:#}");
        "Couldn't save your settings. Check there is free disk space.".to_string()
    })?;
    Ok(s.clone())
}

/// GO lockout (ms) and Stop-all fade (s). Clamped to sensible ranges; applied at once.
#[tauri::command]
pub fn set_audio_options(
    go_lockout_ms: u64,
    stop_fade: f64,
    state: State<'_, AppState>,
) -> Result<Settings, String> {
    let mut s = state.settings.lock().map_err(|_| busy())?;
    let mut next = s.clone();
    next.go_lockout_ms = go_lockout_ms;
    next.stop_fade = stop_fade;
    let next = next.sanitised();
    next.save(&state.settings_path).map_err(|e| {
        tracing::error!("saving settings: {e:#}");
        "Couldn't save your settings. Check there is free disk space.".to_string()
    })?;
    *s = next;
    send(&state, Command::SetOptions(s.engine_options()))?;
    Ok(s.clone())
}

/// The page calls this once it has painted, so the window never shows unstyled.
#[tauri::command]
pub fn app_ready(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
    }
}

// ---------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_shows(state: State<'_, AppState>) -> Result<Vec<ShowSummary>, String> {
    state.library.list().map_err(|e| {
        tracing::error!("listing the library: {e:#}");
        "Couldn't read your show library.".to_string()
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportProgress {
    /// Which import (the file path), so a UI could show several.
    pub path: String,
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportResult {
    pub id: String,
    pub name: String,
    pub cue_count: usize,
    pub warnings: Vec<String>,
}

/// Import a `.tqs` or legacy web `.zip`. Runs off the main thread and reports progress as
/// `import://progress` events.
#[tauri::command]
pub async fn import_file(path: String, app: AppHandle) -> Result<ImportResult, String> {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        let p = PathBuf::from(&path);
        let mut last_emit = std::time::Instant::now();
        let mut on_progress = |pr: tqs::Progress| {
            // At most ~20 events a second, plus the last one.
            if pr.done == pr.total || last_emit.elapsed().as_millis() >= 50 {
                last_emit = std::time::Instant::now();
                let _ = handle.emit(
                    "import://progress",
                    ImportProgress {
                        path: path.clone(),
                        done: pr.done,
                        total: pr.total,
                    },
                );
            }
        };
        match state.library.import_file(&p, &mut on_progress) {
            Ok((id, m, warnings)) => {
                tracing::info!(
                    "imported {} as {id} ({} warnings)",
                    p.display(),
                    warnings.len()
                );
                Ok(ImportResult {
                    id,
                    name: m.show.name,
                    cue_count: m.cues.len(),
                    warnings: warnings.into_iter().map(|w| w.0).collect(),
                })
            }
            Err(e) => {
                tracing::warn!("import of {} refused: {e}", p.display());
                Err(e.to_string())
            }
        }
    })
    .await
    .map_err(|e| {
        tracing::error!("import task failed: {e}");
        "The import stopped unexpectedly. Try again.".to_string()
    })?
}

#[tauri::command]
pub fn delete_show(id: String, state: State<'_, AppState>) -> Result<(), String> {
    {
        let open = state.open_show.lock().map_err(|_| busy())?;
        if open.as_ref().is_some_and(|s| s.id == id) {
            return Err("Close this show before deleting it.".into());
        }
    }
    state.library.delete(&id).map_err(|e| {
        tracing::error!("deleting show {id}: {e:#}");
        "Couldn't move the show to the trash. It is still in your library.".to_string()
    })
}

// ---------------------------------------------------------------------------
// Show screen
// ---------------------------------------------------------------------------

/// What the Show screen draws. The cue list is the manifest's, as normalised by `tqs`.
#[derive(Debug, Clone, Serialize)]
pub struct ShowView {
    pub id: String,
    pub name: String,
    pub author: String,
    pub notes: String,
    pub source: Option<tqs::Source>,
    pub cues: Vec<tqs::Cue>,
    pub local: LocalState,
    /// A sentence when the show was recovered from its backup.
    pub recovered: Option<String>,
}

fn view(s: &crate::library::LoadedShow) -> ShowView {
    ShowView {
        id: s.id.clone(),
        name: s.manifest.show.name.clone(),
        author: s.manifest.show.author.clone(),
        notes: s.manifest.show.notes.clone(),
        source: s.manifest.show.source.clone(),
        cues: s.manifest.cues.clone(),
        local: s.local.clone(),
        recovered: s.recovered.clone(),
    }
}

#[tauri::command]
pub fn open_show(id: String, state: State<'_, AppState>) -> Result<ShowView, String> {
    let loaded = state.library.open(&id).map_err(|e| {
        tracing::error!("opening show {id}: {e:#}");
        "This show can't be opened: its files are damaged and there is no good backup. Import it again.".to_string()
    })?;
    let v = view(&loaded);
    let plan = crate::playback::plan_for(&loaded.dir, &loaded.manifest, loaded.local.standby_index);
    *state.open_dir.lock().map_err(|_| busy())? = Some(loaded.dir.clone());
    *state.open_show.lock().map_err(|_| busy())? = Some(loaded);
    send(&state, Command::LoadShow(plan))?;
    Ok(v)
}

#[tauri::command]
pub fn close_show(state: State<'_, AppState>) -> Result<(), String> {
    // Closing a show stops it.
    send(&state, Command::LoadShow(engine::ShowPlan::default()))?;
    *state.open_dir.lock().map_err(|_| busy())? = None;
    *state.open_show.lock().map_err(|_| busy())? = None;
    Ok(())
}

/// The show that is open, if any: a reloaded webview uses this to come back to it.
#[tauri::command]
pub fn get_open_show(state: State<'_, AppState>) -> Result<Option<ShowView>, String> {
    Ok(state
        .open_show
        .lock()
        .map_err(|_| busy())?
        .as_ref()
        .map(view))
}

// ---------------------------------------------------------------------------
// Transport. Each command is an intent; the engine decides what it means.
// ---------------------------------------------------------------------------

fn send(state: &AppState, cmd: Command) -> Result<(), String> {
    state.engine.send(cmd).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_engine_state(state: State<'_, AppState>) -> engine::Snapshot {
    let s = state.engine.snapshot();
    tracing::debug!(
        "UI asked for engine state: revision {}, standby {}, {} running",
        s.revision,
        s.standby,
        s.running.len()
    );
    s
}

#[tauri::command]
pub fn go(state: State<'_, AppState>) -> Result<(), String> {
    send(&state, Command::Go)
}

#[tauri::command]
pub fn stop_all(panic: bool, state: State<'_, AppState>) -> Result<(), String> {
    send(&state, Command::StopAll { panic })
}

#[tauri::command]
pub fn toggle_pause(state: State<'_, AppState>) -> Result<(), String> {
    send(&state, Command::TogglePause)
}

#[tauri::command]
pub fn set_standby(index: usize, state: State<'_, AppState>) -> Result<(), String> {
    send(&state, Command::SetStandby(index))
}

#[tauri::command]
pub fn move_standby(delta: i32, state: State<'_, AppState>) -> Result<(), String> {
    send(&state, Command::MoveStandby(delta))
}

/// J: play cue number `number` (1-based, as the operator types it).
#[tauri::command]
pub fn jump(number: usize, state: State<'_, AppState>) -> Result<(), String> {
    send(&state, Command::JumpAndPlay(number.saturating_sub(1)))
}

/// Master volume, 0..=1. A playback setting, not part of the show (and not saved, as the web).
#[tauri::command]
pub fn set_master(volume: f64, state: State<'_, AppState>) -> Result<(), String> {
    if !volume.is_finite() {
        return Err("That isn't a volume.".into());
    }
    send(&state, Command::SetMaster(volume.clamp(0.0, 1.0)))
}

#[tauri::command]
pub fn restart_engine(state: State<'_, AppState>) -> Result<(), String> {
    send(&state, Command::RestartAudio)
}
