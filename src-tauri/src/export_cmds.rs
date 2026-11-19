//! Export ▾ and Export audio… (FORMAT.md section 7; thorncue-spec#17). The work is in
//! `audio_export.rs`; these commands pick the show, run it on a blocking task and report
//! progress as `audio-export://progress` events. Nothing here touches the engine thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use tauri::{AppHandle, Emitter, Manager, State};
use tqs::{AudioExportOptions, AudioFormat, Manifest};

use crate::AppState;
use crate::audio_export::{self, Done, Estimate, Progress};
use crate::edit_cmds::blocking;
use crate::saver::save_now;

fn busy() -> String {
    "ThornCue is busy. Try again.".into()
}

/// The show to export, as it is now: the open one (`show` is `None` or its id), or a library
/// show that isn't open (the Library row). Returns its folder and manifest.
fn snapshot(state: &AppState, show: Option<&str>) -> Result<(PathBuf, Manifest), String> {
    {
        let g = state.open_show.lock().map_err(|_| busy())?;
        if let Some(s) = g.as_ref()
            && show.is_none_or(|id| id == s.id)
        {
            return Ok((s.dir.clone(), s.manifest.clone()));
        }
    }
    let id = show.ok_or("No show is open.")?;
    let dir = state
        .library
        .show_dir(id)
        .map_err(|_| "That show is no longer in your library.".to_string())?;
    let (m, _) = crate::library::read_manifest_with_backup(&dir).map_err(|e| {
        tracing::error!("reading {id} for export: {e:#}");
        "This show's files are damaged and there is no good backup, so it can't be exported."
            .to_string()
    })?;
    Ok((dir, m))
}

fn cue_index(m: &Manifest, id: &str) -> Result<usize, String> {
    m.cues
        .iter()
        .position(|c| c.id == id)
        .ok_or_else(|| "That cue is no longer in the show.".to_string())
}

/// The inspector's save dialog name: "<show> Q3 <cue>.wav" (or .mp3).
#[tauri::command]
pub fn cue_audio_file_name(
    id: String,
    format: AudioFormat,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let (_, m) = snapshot(&state, None)?;
    Ok(tqs::single_file_name(&m, cue_index(&m, &id)?, format.ext()))
}

/// Export ▾ → WAV or MP3 for one cue: the audio as it plays (FORMAT.md 7.1).
#[tauri::command]
pub async fn export_cue_audio(
    id: String,
    format: AudioFormat,
    path: String,
    app: AppHandle,
) -> Result<(), String> {
    blocking(move || {
        let (dir, m) = snapshot(&app.state::<AppState>(), None)?;
        let i = cue_index(&m, &id)?;
        audio_export::export_cue(&dir, &m, i, format, Path::new(&path))?;
        tracing::info!("exported cue {id} as {} to {path}", format.ext());
        Ok(())
    })
    .await
}

/// The dialog's summary line (from file headers; no decoding).
#[tauri::command]
pub async fn audio_export_estimate(
    show: Option<String>,
    options: AudioExportOptions,
    app: AppHandle,
) -> Result<Estimate, String> {
    blocking(move || {
        let (dir, m) = snapshot(&app.state::<AppState>(), show.as_deref())?;
        Ok(audio_export::estimate(&dir, &m, &options))
    })
    .await
}

/// The save dialog name: "<show> audio.zip", or "<show> - Q4 to Q9 audio.zip" for part.
#[tauri::command]
pub fn audio_export_file_name(
    show: Option<String>,
    options: AudioExportOptions,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let (_, m) = snapshot(&state, show.as_deref())?;
    Ok(tqs::zip_name(
        &m,
        &options.cues,
        &tqs::picked(&m, &options.cues),
    ))
}

/// Build the audio zip at `path`. Resolves with what was written, or `null` if it was stopped.
/// One export at a time. Pending edits are saved first, so the .tqs inside is current.
#[tauri::command]
pub async fn start_audio_export(
    show: Option<String>,
    options: AudioExportOptions,
    path: String,
    app: AppHandle,
) -> Result<Option<Done>, String> {
    let stop = app
        .state::<AppState>()
        .exports
        .start()
        .ok_or("An audio export is already running.")?;
    let handle = app.clone();
    let r = blocking(move || run(&handle, show.as_deref(), &options, &path, &stop)).await;
    app.state::<AppState>().exports.finish();
    r
}

fn run(
    app: &AppHandle,
    show: Option<&str>,
    options: &AudioExportOptions,
    path: &str,
    stop: &AtomicBool,
) -> Result<Option<Done>, String> {
    save_now(app)?;
    let state = app.state::<AppState>();
    let (dir, m) = snapshot(&state, show)?;
    let work = tempfile::Builder::new()
        .prefix("audio-export-")
        .tempdir_in(&state.tmp_dir)
        .map_err(|e| {
            tracing::error!("export scratch folder: {e}");
            "Couldn't start the export. Check there is free disk space.".to_string()
        })?;
    let mut last = std::time::Instant::now();
    let mut progress = |p: Progress| {
        // At most ~10 events a second, plus every phase change and the last one.
        if p.done == p.total || p.done <= 1 || last.elapsed().as_millis() >= 100 {
            last = std::time::Instant::now();
            let _ = app.emit("audio-export://progress", p);
        }
    };
    let r = audio_export::build_zip(
        &dir,
        &m,
        options,
        Path::new(path),
        work.path(),
        stop,
        &mut progress,
    );
    match &r {
        Ok(Some(d)) => tracing::info!("audio export: {} files to {path}", d.files),
        Ok(None) => tracing::info!("audio export to {path} stopped"),
        Err(e) => tracing::warn!("audio export to {path} failed: {e}"),
    }
    r
}

/// Stop the running export (the dialog's Stop). It ends at the next cue or 1 MB of the zip.
#[tauri::command]
pub fn stop_audio_export(state: State<'_, AppState>) {
    state.exports.stop();
}

/// What the Export audio dialog shows about a show: its cues (name, group, whether it has an
/// audio file and of what kind) and groups. For the Library row too, where the show isn't open.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Outline {
    pub name: String,
    pub cues: Vec<OutlineCue>,
    pub groups: Vec<tqs::Group>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OutlineCue {
    pub id: String,
    pub name: String,
    pub group: Option<String>,
    /// The original file's extension, lowercase, when it's an audio cue whose file is there.
    pub audio_ext: Option<String>,
}

#[tauri::command]
pub fn audio_export_outline(
    show: Option<String>,
    state: State<'_, AppState>,
) -> Result<Outline, String> {
    let (dir, m) = snapshot(&state, show.as_deref())?;
    let cues = m
        .cues
        .iter()
        .map(|c| OutlineCue {
            id: c.id.clone(),
            name: c.name.clone(),
            group: c.group.clone(),
            audio_ext: (c.cue_type == tqs::CueType::Audio)
                .then(|| c.asset.as_ref().and_then(|a| m.assets.get(a)))
                .flatten()
                .filter(|a| dir.join(&a.path).is_file())
                .map(|a| {
                    Path::new(&a.path)
                        .extension()
                        .map(|e| e.to_string_lossy().to_lowercase())
                        .unwrap_or_default()
                }),
        })
        .collect();
    Ok(Outline {
        name: m.show.name.clone(),
        cues,
        groups: m.groups.clone(),
    })
}
