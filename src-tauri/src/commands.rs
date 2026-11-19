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
    Ok(state.settings.lock().map_err(|_| busy())?.for_ui())
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
    Ok(s.for_ui())
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
    Ok(s.for_ui())
}

fn save_settings(state: &AppState, next: &Settings) -> Result<(), String> {
    next.save(&state.settings_path).map_err(|e| {
        tracing::error!("saving settings: {e:#}");
        "Couldn't save your settings. Check there is free disk space.".to_string()
    })
}

/// Show and audition outputs (thorncue-spec#9). The webview asks the operator first before
/// sending anything that makes auditions play through the show output; the engine applies the
/// blocking rules either way.
#[tauri::command]
pub fn set_outputs(
    show: engine::ShowOutput,
    audition: engine::AuditionOutput,
    state: State<'_, AppState>,
) -> Result<Settings, String> {
    let mut s = state.settings.lock().map_err(|_| busy())?;
    let mut next = s.clone();
    next.show_output = show.clone();
    next.audition_output = audition.clone();
    save_settings(&state, &next)?;
    *s = next;
    tracing::info!("outputs: show {show:?}, audition {audition:?}");
    send(&state, Command::SetOutputs { show, audition })?;
    Ok(s.for_ui())
}

/// Audition volume (0..=1), separate from the master. Saved per machine.
#[tauri::command]
pub fn set_audition_volume(volume: f64, state: State<'_, AppState>) -> Result<(), String> {
    let mut s = state.settings.lock().map_err(|_| busy())?;
    let mut next = s.clone();
    next.audition_volume = volume;
    let next = next.sanitised();
    save_settings(&state, &next)?;
    *s = next;
    send(&state, Command::SetAuditionVolume(s.audition_volume))
}

/// List the output devices again (the outputs bar opened its device lists).
#[tauri::command]
pub fn refresh_devices(state: State<'_, AppState>) {
    state.engine.refresh_devices();
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
    pub groups: Vec<tqs::Group>,
    pub local: LocalState,
    /// A sentence when the show was recovered from its backup.
    pub recovered: Option<String>,
}

pub(crate) fn view(s: &crate::library::LoadedShow) -> ShowView {
    ShowView {
        id: s.id.clone(),
        name: s.manifest.show.name.clone(),
        author: s.manifest.show.author.clone(),
        notes: s.manifest.show.notes.clone(),
        source: s.manifest.show.source.clone(),
        cues: s.manifest.cues.clone(),
        groups: s.manifest.groups.clone(),
        local: s.local.clone(),
        recovered: s.recovered.clone(),
    }
}

#[tauri::command]
pub fn open_show(
    id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ShowView, String> {
    // Switching shows: save the one that's open first.
    crate::saver::save_now(&app)?;
    let loaded = state.library.open(&id).map_err(|e| {
        tracing::error!("opening show {id}: {e:#}");
        "This show can't be opened: its files are damaged and there is no good backup. Import it again.".to_string()
    })?;
    let v = view(&loaded);
    let plan = crate::playback::plan_for(&loaded.dir, &loaded.manifest, loaded.local.standby_index);
    *state.open_dir.lock().map_err(|_| busy())? = Some(loaded.dir.clone());
    *state.open_show.lock().map_err(|_| busy())? = Some(loaded);
    send(&state, Command::LoadShow(plan))?;
    if let Some(warning) = state.session.show_opened(&id) {
        let _ = app.emit(
            "engine://notice",
            engine::Notice {
                text: warning,
                error: true,
            },
        );
    }
    Ok(v)
}

/// After an unclean exit (crash, kill, power cut): the show that was open, opened again at its
/// standby cue, with a sentence saying so. Nothing plays. Only the first call after start gets
/// it, and not once the operator has opened a show.
#[derive(Debug, Clone, Serialize)]
pub struct Recovered {
    pub show: ShowView,
    pub message: String,
}

#[tauri::command]
pub fn recover_session(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<Recovered>, String> {
    if state.open_show.lock().map_err(|_| busy())?.is_some() {
        return Ok(None);
    }
    let Some(id) = state.session.take_recovered() else {
        return Ok(None);
    };
    if !state
        .library
        .list()
        .map_err(|_| busy())?
        .iter()
        .any(|s| s.id == id)
    {
        tracing::warn!("the show to recover ({id}) is no longer in the library");
        return Ok(None);
    }
    let mut show = open_show(id, app, state.clone())?;
    let standby = show.local.standby_index;
    let at = match show.cues.get(standby) {
        Some(c) => format!(
            "{} {} is on standby",
            tqs::cue_number(standby, show.cues.len(), false),
            c.name
        ),
        None => "standby is at the end of the cue list".to_string(),
    };
    let message = format!(
        "ThornCue didn't close properly last time, so \"{}\" is open again and {at}. Nothing is playing.",
        show.name
    );
    // Shown as the Show screen's banner (with any backup notice), kept for this session.
    let banner = match show.recovered.take() {
        Some(b) => format!("{message} {b}"),
        None => message.clone(),
    };
    if let Some(s) = state.open_show.lock().map_err(|_| busy())?.as_mut() {
        s.recovered = Some(banner.clone());
    }
    show.recovered = Some(banner);
    Ok(Some(Recovered { show, message }))
}

#[tauri::command]
pub fn close_show(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    crate::saver::save_now(&app)?;
    // Closing a show stops it.
    send(&state, Command::LoadShow(engine::ShowPlan::default()))?;
    *state.open_dir.lock().map_err(|_| busy())? = None;
    *state.open_show.lock().map_err(|_| busy())? = None;
    state.session.show_closed();
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

// ---------------------------------------------------------------------------
// Sample show and first-run welcome (thorncue-spec#11)
// ---------------------------------------------------------------------------

/// The welcome card's Skip: don't show it again.
#[tauri::command]
pub fn skip_welcome(state: State<'_, AppState>) -> Result<Settings, String> {
    let mut s = state.settings.lock().map_err(|_| busy())?;
    let mut next = s.clone();
    next.welcome_seen = true;
    save_settings(&state, &next)?;
    *s = next;
    Ok(s.for_ui())
}

/// Run a sample-show action off the main thread (it imports ~4 MB), then save the settings it
/// changed. Returns the sample's library id.
async fn sample_action(
    app: AppHandle,
    what: &'static str,
    f: fn(&crate::library::Library, &mut Settings, &std::path::Path) -> anyhow::Result<String>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let mut s = state.settings.lock().map_err(|_| busy())?.clone();
        let id = f(&state.library, &mut s, &state.tmp_dir).map_err(|e| {
            tracing::error!("{what} the sample show: {e:#}");
            "Couldn't set up the sample show. Check there is free disk space.".to_string()
        })?;
        let mut live = state.settings.lock().map_err(|_| busy())?;
        let mut next = live.clone();
        next.welcome_seen = s.welcome_seen;
        next.sample_show_id = s.sample_show_id;
        save_settings(&state, &next)?;
        *live = next;
        Ok(id)
    })
    .await
    .map_err(|e| {
        tracing::error!("sample show task failed: {e}");
        "That stopped unexpectedly. Try again.".to_string()
    })?
}

/// The sample show's id, importing a fresh copy if it isn't in the library. The page opens it.
#[tauri::command]
pub async fn open_sample(app: AppHandle) -> Result<String, String> {
    sample_action(app, "opening", |lib, s, tmp| {
        crate::sample::open(lib, s, tmp).map(|(id, _)| id)
    })
    .await
}

/// Replace the sample show with a fresh copy (the page asks first). Refused while it's open.
#[tauri::command]
pub async fn reset_sample(app: AppHandle) -> Result<String, String> {
    {
        let state = app.state::<AppState>();
        let sample = state
            .settings
            .lock()
            .map_err(|_| busy())?
            .sample_show_id
            .clone();
        let open = state.open_show.lock().map_err(|_| busy())?;
        if sample.is_some() && open.as_ref().map(|o| &o.id) == sample.as_ref() {
            return Err("Close the sample show before resetting it.".into());
        }
    }
    sample_action(app, "resetting", crate::sample::reset).await
}
