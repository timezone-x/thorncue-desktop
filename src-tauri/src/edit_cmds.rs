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

// ---- Groups (thorncue-spec#2) ----

#[tauri::command]
pub fn create_group(
    ids: Vec<String>,
    name: String,
    color: String,
    app: AppHandle,
) -> Result<EditResult, String> {
    edit(&app, None, Edit::CreateGroup { ids, name, color }, vec![])
}

#[tauri::command]
pub fn edit_group(
    id: String,
    name: String,
    color: String,
    app: AppHandle,
) -> Result<EditResult, String> {
    edit(&app, None, Edit::UpdateGroup { id, name, color }, vec![])
}

#[tauri::command]
pub fn ungroup(id: String, app: AppHandle) -> Result<EditResult, String> {
    edit(&app, None, Edit::Ungroup { id }, vec![])
}

/// `set_group`: false leaves membership alone (a whole group dragged); true sets `group`.
#[tauri::command]
pub fn move_cues(
    ids: Vec<String>,
    anchor: String,
    after: bool,
    set_group: bool,
    group: Option<String>,
    app: AppHandle,
) -> Result<EditResult, String> {
    let group = set_group.then_some(group);
    edit(
        &app,
        None,
        Edit::MoveCues {
            ids,
            anchor,
            after,
            group,
        },
        vec![],
    )
}

#[tauri::command]
pub fn set_cue_group(
    id: String,
    group: Option<String>,
    app: AppHandle,
) -> Result<EditResult, String> {
    edit(&app, None, Edit::SetGroup { id, group }, vec![])
}

/// Collapse or expand a group on this machine (local.json, never the show file).
#[tauri::command]
pub fn set_group_collapsed(
    id: String,
    collapsed: bool,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let mut guard = state.open_show.lock().map_err(|_| busy())?;
    let s = guard.as_mut().ok_or("No show is open.")?;
    let known: Vec<&str> = s.manifest.groups.iter().map(|g| g.id.as_str()).collect();
    let mut list: Vec<String> = s
        .local
        .collapsed_groups
        .iter()
        .filter(|g| known.contains(&g.as_str()) && **g != id)
        .cloned()
        .collect();
    if collapsed && known.contains(&id.as_str()) {
        list.push(id);
    }
    s.local.collapsed_groups = list.clone();
    let dir = s.dir.clone();
    drop(guard);
    // A view setting: if it can't be written, it's only forgotten next time.
    if let Err(e) = crate::library::update_local(&dir, |l| l.collapsed_groups = list.clone()) {
        tracing::warn!("couldn't remember collapsed groups: {e:#}");
    }
    Ok(list)
}

// ---- .tqc and .tqg (thorncue-spec#3, FORMAT.md 6) ----

/// What a `.tqc` or `.tqg` import added, for the toast.
#[derive(Debug, Clone, Serialize)]
pub struct PieceImport {
    pub file: String,
    pub kind: &'static str,
    pub cues: usize,
    pub group: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PiecesResult {
    pub show: crate::commands::ShowView,
    pub added: Vec<PieceImport>,
    /// One sentence per file that couldn't be imported.
    pub problems: Vec<String>,
}

/// Add `.tqc`/`.tqg` files at the end of the open show (FORMAT.md 6.4). Each file is checked
/// whole, into a temp folder inside the show, before anything is added; a bad file is skipped
/// and named, the rest still go in. Standby and running cues aren't touched.
#[tauri::command]
pub async fn import_pieces(paths: Vec<String>, app: AppHandle) -> Result<PiecesResult, String> {
    blocking(move || {
        let (id, dir) = open_dir(&app.state::<AppState>())?;
        let mut added = Vec::new();
        let mut problems = Vec::new();
        let mut show = None;
        for p in paths {
            let path = PathBuf::from(&p);
            let file = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or(p.clone());
            match import_piece(&app, &id, &dir, &path) {
                Ok((v, kind, cues, group)) => {
                    show = Some(v);
                    added.push(PieceImport {
                        file,
                        kind,
                        cues,
                        group,
                    });
                }
                Err(e) => problems.push(format!("Couldn't import {file}: {e}")),
            }
        }
        let show = match show {
            Some(v) => v,
            None if problems.is_empty() => {
                return Err("Choose a .tqc (cues) or .tqg (group) file.".into());
            }
            None => return Err(problems.join(" ")),
        };
        Ok(PiecesResult {
            show,
            added,
            problems,
        })
    })
    .await
}

type Imported = (
    crate::commands::ShowView,
    &'static str,
    usize,
    Option<String>,
);

fn import_piece(
    app: &AppHandle,
    show_id: &str,
    dir: &std::path::Path,
    path: &std::path::Path,
) -> Result<Imported, String> {
    let stage = tempfile::Builder::new()
        .prefix(".import-piece-")
        .tempdir_in(dir)
        .map_err(|e| {
            format!("couldn't prepare the show folder ({e}). Check there is free disk space.")
        })?;
    let unpacked = stage.path().join("pkg");
    let (piece, _warnings) = tqs::read_package_of(
        path,
        &unpacked,
        tqs::Limits::default(),
        &[tqs::Kind::Cues, tqs::Kind::Group],
        &mut |_| {},
    )
    .map_err(|e| {
        let msg = e.to_string();
        if msg.starts_with("This is a show (.tqs)") {
            "it's a whole show (.tqs). Import it from the Library instead.".to_string()
        } else {
            tracing::warn!("importing {}: {msg}", path.display());
            lower_first(&msg)
        }
    })?;
    let prepared = {
        let state = app.state::<AppState>();
        let g = state.open_show.lock().map_err(|_| busy())?;
        let s = g.as_ref().ok_or("no show is open.")?;
        if s.id != show_id {
            return Err("the show changed while the file was being read. Import it again.".into());
        }
        editing::prepare_piece(&s.manifest, dir, &piece, &unpacked)
    };
    // Move the audio in, then add the cues; if the edit is refused, take the audio out again.
    let mut moved = Vec::new();
    for (from, rel) in &prepared.files {
        let to = dir.join(rel);
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::rename(from, &to) {
            for m in &moved {
                let _ = std::fs::remove_file(m);
            }
            tracing::error!("moving {} into the show: {e}", from.display());
            return Err(
                "couldn't copy its audio into the show. Check there is free disk space.".into(),
            );
        }
        moved.push(to);
    }
    crate::fsutil::sync_dir(&dir.join("audio"));
    let kind = if prepared.kind == tqs::Kind::Group {
        "group"
    } else {
        "cues"
    };
    let n = prepared.cues.len();
    let group = prepared
        .group
        .as_ref()
        .and_then(|g| g["name"].as_str())
        .map(String::from);
    match edit(app, Some(show_id), Edit::AddPiece(prepared), vec![]) {
        Ok(r) => Ok((r.show, kind, n, group)),
        Err(e) => {
            for m in &moved {
                let _ = std::fs::remove_file(m);
            }
            Err(lower_first(&e))
        }
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) if !s.starts_with("ThornCue") => f.to_lowercase().chain(c).collect(),
        _ => s.to_string(),
    }
}

/// The file name the save dialog suggests, as the web's build_package: one cue is
/// `<cue name>.tqc`, a range `<show> - Q4 to Q9.tqc`, a group `<group name>.tqg`.
#[tauri::command]
pub fn piece_file_name(
    ids: Option<Vec<String>>,
    group: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let g = state.open_show.lock().map_err(|_| busy())?;
    let s = g.as_ref().ok_or("No show is open.")?;
    let m = &s.manifest;
    let safe = crate::library::safe_file_name;
    if let Some(gid) = group {
        let name = m
            .groups
            .iter()
            .find(|g| g.id == gid)
            .map(|g| g.name.as_str())
            .unwrap_or("Group");
        return Ok(format!("{}.tqg", safe(name)));
    }
    let ids = ids.unwrap_or_default();
    let pos: Vec<usize> = (0..m.cues.len())
        .filter(|&i| ids.contains(&m.cues[i].id))
        .collect();
    match pos.as_slice() {
        [] => Err("Select a cue first.".into()),
        [i] => {
            let name = m.cues[*i].name.trim();
            let name = if name.is_empty() {
                format!(
                    "{} {}",
                    m.show.name,
                    tqs::cue_number(*i, m.cues.len(), false)
                )
            } else {
                name.to_string()
            };
            Ok(format!("{}.tqc", safe(&name)))
        }
        [a, .., b] => Ok(format!(
            "{}.tqc",
            safe(&format!(
                "{} - {} to {}",
                m.show.name,
                tqs::cue_number(*a, m.cues.len(), false),
                tqs::cue_number(*b, m.cues.len(), false)
            ))
        )),
    }
}

/// Write the picked cues as a `.tqc`, or a group as a `.tqg`, saving pending edits first.
#[tauri::command]
pub async fn export_piece(
    ids: Option<Vec<String>>,
    group: Option<String>,
    path: String,
    app: AppHandle,
) -> Result<(), String> {
    blocking(move || {
        save_now(&app)?;
        let (_, dir) = open_dir(&app.state::<AppState>())?;
        let piece = match (group, ids) {
            (Some(g), _) => tqs::Piece::Group(g),
            (None, Some(ids)) if !ids.is_empty() => tqs::Piece::Cues(ids),
            _ => return Err("Select a cue first.".into()),
        };
        tqs::write_piece(&dir, &PathBuf::from(&path), &piece).map(|_| ()).map_err(|e| {
            tracing::error!("exporting {piece:?} to {path}: {e}");
            match e {
                tqs::TqsError::Invalid(m) => m,
                _ => "Couldn't write the file. Check the folder exists and there is free disk space.".to_string(),
            }
        })
    })
    .await
}

#[tauri::command]
pub fn set_show_name(name: String, app: AppHandle) -> Result<EditResult, String> {
    edit(&app, None, Edit::ShowName(name), vec![])
}

pub(crate) async fn blocking<T: Send + 'static>(
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
        let (files, problems) =
            editing::copy_many(&dir, paths.into_iter().map(PathBuf::from).collect());
        if files.is_empty() {
            return Err(if problems.len() == 1 {
                problems.join(" ")
            } else {
                format!("Couldn't add any of the files. {}", problems.join(" "))
            });
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
