//! The library on disk (docs/ARCHITECTURE.md "Files and the library").
//!
//! ```text
//! <app_data_dir>/library/<show-id>/manifest.json       the show (an unpacked .tqs)
//! <app_data_dir>/library/<show-id>/manifest.json.bak   last known-good copy
//! <app_data_dir>/library/<show-id>/audio/<asset>.<ext>
//! <app_data_dir>/library/<show-id>/local.json          client-only state
//! ```
//!
//! Imports are unpacked by `tqs` into a hidden temp folder inside `library/` (same
//! filesystem) and renamed into place only once fully validated.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tqs::{Limits, Manifest, PackageKind, Progress, Warning};

use crate::fsutil::atomic_write;

/// Client-only state for one show. Not part of the show, never exported.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LocalState {
    pub last_opened: Option<String>,
    pub standby_index: usize,
    pub imported_at: Option<String>,
    pub imported_revision: Option<String>,
    pub modified_since_import: bool,
    /// The server and show code this copy was downloaded from, as the operator reached it
    /// (a LAN address can differ from the public URL in the manifest's `source`).
    pub web_server: Option<String>,
    pub web_code: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Where a downloaded package came from.
#[derive(Debug, Clone)]
pub struct WebOrigin {
    pub server: String,
    pub code: String,
    /// The revision downloaded (the response ETag), compared with the web's list.
    pub revision: Option<String>,
}

/// One row of the Library table.
#[derive(Debug, Clone, Serialize)]
pub struct ShowSummary {
    pub id: String,
    pub name: String,
    pub cue_count: usize,
    pub audio_bytes: u64,
    /// "ThornCue Web (STAGE-4821)", "File" etc.
    pub source: Option<SourceSummary>,
    pub last_opened: Option<String>,
    pub imported_at: Option<String>,
    /// Set when `manifest.json` couldn't be read and the backup was used.
    pub damaged: bool,
    /// The web revision this copy was imported at.
    pub imported_revision: Option<String>,
    pub web_server: Option<String>,
    pub web_code: Option<String>,
    /// Edited here since it was imported.
    pub modified_since_import: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceSummary {
    pub server: String,
    pub code: String,
}

/// A show loaded for the Show screen.
#[derive(Debug, Clone)]
pub struct LoadedShow {
    pub id: String,
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub local: LocalState,
    /// When the backup had to be used: a sentence for the user.
    pub recovered: Option<String>,
    /// Edited since the last save.
    pub dirty: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ReplaceError {
    #[error("That show is no longer in your library.")]
    NoSuchShow,
    #[error("{0}")]
    Package(tqs::TqsError),
    #[error(
        "Couldn't swap in the new copy. Your existing show is unchanged. Check there is free disk space."
    )]
    Disk,
}

pub struct Library {
    root: PathBuf,
}

impl Library {
    pub fn new(app_data_dir: &Path) -> Result<Self> {
        let root = app_data_dir.join("library");
        std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
        let lib = Self { root };
        lib.clean_leftovers();
        Ok(lib)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Imports interrupted by a crash leave `.tqs-import-*` folders; remove them on start.
    fn clean_leftovers(&self) {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(".tqs-import-")
                || name.starts_with(".replace-")
                || name.starts_with(".new-")
            {
                tracing::info!("removing an unfinished import: {}", e.path().display());
                let _ = std::fs::remove_dir_all(e.path());
            } else if let Some(id) = name.strip_prefix(".old-") {
                // A Replace was interrupted between its two renames: if the new copy didn't
                // make it into place, put the old one back; otherwise it goes to the trash.
                let live = self.root.join(id);
                if live.exists() {
                    if let Err(err) = trash::delete(e.path()) {
                        tracing::warn!("couldn't trash {}: {err}", e.path().display());
                    }
                } else {
                    tracing::warn!("restoring {id} after an interrupted replace");
                    let _ = std::fs::rename(e.path(), live);
                }
            }
        }
    }

    /// Library shows imported from `server` + `code` (matching spellings of the server).
    pub fn find_by_source(&self, server: &str, code: &str) -> Vec<ShowSummary> {
        self.list()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| {
                let by_manifest = s.source.as_ref().is_some_and(|src| {
                    src.code == code && crate::web::ServerUrl::same_server(&src.server, server)
                });
                let by_download = s.web_code.as_deref() == Some(code)
                    && s.web_server
                        .as_deref()
                        .is_some_and(|w| crate::web::ServerUrl::same_server(w, server));
                by_manifest || by_download
            })
            .collect()
    }

    /// Import a downloaded `.tqs` as a new show.
    pub fn import_package(
        &self,
        file: &Path,
        origin: &WebOrigin,
        on_progress: &mut dyn FnMut(Progress),
    ) -> std::result::Result<(String, Manifest, Vec<Warning>), tqs::TqsError> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let dest = self.root.join(&id);
        let (manifest, warnings) =
            tqs::read_package_with_progress(file, &dest, Limits::default(), on_progress)?;
        let local = LocalState {
            imported_at: Some(tqs::now_rfc3339()),
            imported_revision: revision_of(origin, &manifest),
            web_server: Some(origin.server.clone()),
            web_code: Some(origin.code.clone()),
            ..Default::default()
        };
        if let Err(e) = write_local(&dest, &local) {
            tracing::warn!("couldn't write local.json for {id}: {e:#}");
        }
        Ok((id, manifest, warnings))
    }

    /// Replace show `id`'s content with a downloaded `.tqs`, keeping its id, standby and last
    /// opened. The old content goes to the OS trash. Nothing changes if the package is refused.
    pub fn replace_with_package(
        &self,
        id: &str,
        file: &Path,
        origin: &WebOrigin,
        on_progress: &mut dyn FnMut(Progress),
    ) -> std::result::Result<(Manifest, Vec<Warning>), ReplaceError> {
        let live = self.show_dir(id).map_err(|_| ReplaceError::NoSuchShow)?;
        if !live.is_dir() {
            return Err(ReplaceError::NoSuchShow);
        }
        let incoming = self
            .root
            .join(format!(".replace-{}", uuid::Uuid::new_v4().simple()));
        let (manifest, warnings) =
            tqs::read_package_with_progress(file, &incoming, Limits::default(), on_progress)
                .map_err(ReplaceError::Package)?;
        let old_local = read_local(&live);
        let old = self.root.join(format!(".old-{id}"));
        let swap = || -> Result<()> {
            std::fs::rename(&live, &old).context("moving the old copy aside")?;
            if let Err(e) = std::fs::rename(&incoming, &live) {
                let _ = std::fs::rename(&old, &live);
                return Err(e).context("moving the new copy into place");
            }
            Ok(())
        };
        if let Err(e) = swap() {
            let _ = std::fs::remove_dir_all(&incoming);
            tracing::error!("replacing {id}: {e:#}");
            return Err(ReplaceError::Disk);
        }
        crate::fsutil::sync_dir(&self.root);
        let local = LocalState {
            standby_index: old_local.standby_index.min(manifest.cues.len()),
            last_opened: old_local.last_opened,
            imported_at: Some(tqs::now_rfc3339()),
            imported_revision: revision_of(origin, &manifest),
            modified_since_import: false,
            web_server: Some(origin.server.clone()),
            web_code: Some(origin.code.clone()),
            extra: old_local.extra,
        };
        if let Err(e) = write_local(&live, &local) {
            tracing::warn!("couldn't write local.json for {id}: {e:#}");
        }
        // Give the old copy a name an operator can find in the trash (a leading dot hides it).
        let readable = self.root.join(format!(
            "{} (replaced {})",
            safe_file_name(&manifest.show.name),
            tqs::now_rfc3339().replace(':', "-").trim_end_matches('Z')
        ));
        let to_trash = if std::fs::rename(&old, &readable).is_ok() {
            readable
        } else {
            old
        };
        if let Err(e) = trash::delete(&to_trash) {
            tracing::warn!("couldn't move the old copy of {id} to the trash: {e}");
        }
        Ok((manifest, warnings))
    }

    pub fn show_dir(&self, id: &str) -> Result<PathBuf> {
        // Ids are ours (uuid simple), but they come back from the webview: check them.
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("not a show id: {id:?}");
        }
        Ok(self.root.join(id))
    }

    pub fn list(&self) -> Result<Vec<ShowSummary>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(&self.root)?.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || !e.path().is_dir() {
                continue;
            }
            match self.summary(&name) {
                Ok(s) => out.push(s),
                Err(err) => tracing::warn!("skipping library folder {name}: {err:#}"),
            }
        }
        // Most recently used first, then by name.
        out.sort_by(|a, b| {
            b.last_opened
                .cmp(&a.last_opened)
                .then_with(|| b.imported_at.cmp(&a.imported_at))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(out)
    }

    fn summary(&self, id: &str) -> Result<ShowSummary> {
        let dir = self.show_dir(id)?;
        let (manifest, recovered) = read_manifest_with_backup(&dir)?;
        let local = read_local(&dir);
        Ok(ShowSummary {
            id: id.to_string(),
            name: manifest.show.name.clone(),
            cue_count: manifest.cues.len(),
            audio_bytes: manifest.audio_bytes(),
            source: manifest.show.source.as_ref().map(|s| SourceSummary {
                server: s.server.clone(),
                code: s.code.clone(),
            }),
            last_opened: local.last_opened,
            imported_at: local.imported_at,
            damaged: recovered.is_some(),
            imported_revision: local.imported_revision,
            modified_since_import: local.modified_since_import,
            web_server: local.web_server,
            web_code: local.web_code,
        })
    }

    /// Import a `.tqs` or a legacy web `.zip` into a new library folder.
    pub fn import_file(
        &self,
        path: &Path,
        on_progress: &mut dyn FnMut(Progress),
    ) -> std::result::Result<(String, Manifest, Vec<Warning>), tqs::TqsError> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let dest = self.root.join(&id);
        let (manifest, warnings) = match tqs::detect_kind(path)? {
            PackageKind::Show => {
                tqs::read_package_with_progress(path, &dest, Limits::default(), on_progress)?
            }
            PackageKind::LegacyWebExport => tqs::import_legacy_zip(path, &dest, Limits::default())?,
        };
        let local = LocalState {
            imported_at: Some(tqs::now_rfc3339()),
            imported_revision: manifest.show.source.as_ref().map(|s| s.revision.clone()),
            ..Default::default()
        };
        if let Err(e) = write_local(&dest, &local) {
            // The show itself is in place; client state can be rebuilt.
            tracing::warn!("couldn't write local.json for {id}: {e:#}");
        }
        Ok((id, manifest, warnings))
    }

    /// A new, empty show. Returns its id.
    pub fn create_show(&self, name: &str) -> Result<String> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let tmp = self.root.join(format!(".new-{id}"));
        std::fs::create_dir_all(tmp.join("audio"))?;
        let m = crate::editing::empty_show(name);
        atomic_write(&tmp.join("manifest.json"), &m.to_json_pretty())?;
        write_local(&tmp, &LocalState::default())?;
        std::fs::rename(&tmp, self.root.join(&id))?;
        crate::fsutil::sync_dir(&self.root);
        Ok(id)
    }

    /// Copy a show (its audio too) as a new show named "<name> copy". The copy is a new show,
    /// so it no longer claims to be the web's: `source` is cleared. Returns the new id.
    pub fn duplicate_show(&self, id: &str) -> Result<String> {
        let src = self.show_dir(id)?;
        let (mut manifest, _) = read_manifest_with_backup(&src)?;
        let new_id = uuid::Uuid::new_v4().simple().to_string();
        let tmp = self.root.join(format!(".new-{new_id}"));
        let result = (|| -> Result<()> {
            std::fs::create_dir_all(tmp.join("audio"))?;
            for a in manifest.assets.values() {
                std::fs::copy(src.join(&a.path), tmp.join(&a.path))
                    .with_context(|| format!("copying {}", a.path))?;
            }
            manifest.show.name = format!("{} copy", manifest.show.name)
                .chars()
                .take(100)
                .collect();
            manifest.show.source = None;
            atomic_write(&tmp.join("manifest.json"), &manifest.to_json_pretty())?;
            write_local(&tmp, &LocalState::default())?;
            std::fs::rename(&tmp, self.root.join(&new_id))?;
            Ok(())
        })();
        if let Err(e) = result {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
        crate::fsutil::sync_dir(&self.root);
        Ok(new_id)
    }

    /// Make sure a show's manifest.json is readable (recovering it from .bak if not), e.g.
    /// before exporting a show that isn't open. Returns the recovery sentence, if any.
    pub fn ensure_readable(&self, id: &str) -> Result<Option<String>> {
        let dir = self.show_dir(id)?;
        let (manifest, recovered) = read_manifest_with_backup(&dir)?;
        if recovered.is_some() {
            repair(&dir, &manifest);
        }
        Ok(recovered)
    }

    /// Move a show's folder to the OS trash.
    pub fn delete(&self, id: &str) -> Result<()> {
        let dir = self.show_dir(id)?;
        if !dir.is_dir() {
            bail!("no such show {id}");
        }
        trash::delete(&dir).with_context(|| format!("moving {} to the trash", dir.display()))
    }

    /// Load a show for the Show screen, recovering from `.bak` if needed.
    pub fn open(&self, id: &str) -> Result<LoadedShow> {
        let dir = self.show_dir(id)?;
        let (manifest, recovered) = read_manifest_with_backup(&dir)?;
        let mut local = read_local(&dir);
        if recovered.is_none() {
            // The file just parsed, so it is a known-good copy.
            if let Err(e) = refresh_backup(&dir) {
                tracing::warn!("couldn't refresh manifest.json.bak for {id}: {e:#}");
            }
        } else {
            repair(&dir, &manifest);
        }
        local.standby_index = local.standby_index.min(manifest.cues.len());
        local.last_opened = Some(tqs::now_rfc3339());
        if let Err(e) = write_local(&dir, &local) {
            tracing::warn!("couldn't update local.json for {id}: {e:#}");
        }
        Ok(LoadedShow {
            id: id.to_string(),
            dir,
            manifest,
            local,
            recovered,
            dirty: false,
        })
    }
}

/// A show name usable as a folder name on every OS.
pub fn safe_file_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(60)
        .collect();
    let s = s
        .trim()
        .trim_end_matches('.')
        .trim_start_matches('.')
        .to_string();
    if s.is_empty() { "Show".into() } else { s }
}

fn revision_of(origin: &WebOrigin, manifest: &Manifest) -> Option<String> {
    origin
        .revision
        .clone()
        .or_else(|| manifest.show.source.as_ref().map(|s| s.revision.clone()))
}

/// Put a recovered manifest back as manifest.json. The damaged file is kept beside it as
/// `manifest.json.damaged-<time>` (never delete what might be someone's work).
fn repair(dir: &Path, manifest: &Manifest) {
    let damaged = dir.join(format!(
        "manifest.json.damaged-{}",
        tqs::now_rfc3339().replace(':', "-")
    ));
    let main = dir.join("manifest.json");
    if main.exists()
        && let Err(e) = std::fs::rename(&main, &damaged)
    {
        tracing::warn!(
            "couldn't set the damaged manifest aside in {}: {e}",
            dir.display()
        );
    }
    if let Err(e) = atomic_write(&main, &manifest.to_json_pretty()) {
        tracing::warn!(
            "couldn't write the recovered manifest in {}: {e:#}",
            dir.display()
        );
    }
}

/// Read `manifest.json`; if it is missing or damaged, fall back to `.bak` and say so.
pub fn read_manifest_with_backup(dir: &Path) -> Result<(Manifest, Option<String>)> {
    let main = dir.join("manifest.json");
    let primary = std::fs::read(&main)
        .map_err(anyhow::Error::from)
        .and_then(|b| Ok(Manifest::from_slice(&b)?.0));
    match primary {
        Ok(m) => Ok((m, None)),
        Err(e) => {
            tracing::warn!("{} is damaged: {e:#}; trying the backup", main.display());
            let bak = dir.join("manifest.json.bak");
            let bytes =
                std::fs::read(&bak).with_context(|| format!("no backup at {}", bak.display()))?;
            let (m, _) = Manifest::from_slice(&bytes).context("the backup is damaged too")?;
            let when = std::fs::metadata(&bak)
                .and_then(|md| md.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| format!(" (saved {})", tqs_time(d.as_secs())))
                .unwrap_or_default();
            let msg = format!(
                "\"{}\" was damaged, so ThornCue opened the last good copy{when}. Check your most recent changes.",
                m.show.name
            );
            Ok((m, Some(msg)))
        }
    }
}

/// "5 minutes ago", for telling the user how old a backup is.
fn tqs_time(secs: u64) -> String {
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    let ago = std::time::SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match ago {
        0..=89 => "just now".into(),
        90..=3599 => format!("{} minutes ago", ago / 60),
        3600..=86_399 => format!("{} hours ago", ago / 3600),
        _ => format!("{} days ago", ago / 86_400),
    }
}

/// Copy the current (good) `manifest.json` to `.bak`, atomically.
pub fn refresh_backup(dir: &Path) -> Result<()> {
    let bytes = std::fs::read(dir.join("manifest.json"))?;
    atomic_write(&dir.join("manifest.json.bak"), &bytes)
}

pub fn read_local(dir: &Path) -> LocalState {
    std::fs::read(dir.join("local.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn write_local(dir: &Path, local: &LocalState) -> Result<()> {
    let _guard = LOCAL_LOCK.lock();
    atomic_write(&dir.join("local.json"), &serde_json::to_vec_pretty(local)?)
}

/// Several threads touch local.json (standby saver, autosave): read-modify-write under one lock.
static LOCAL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn update_local(dir: &Path, f: impl FnOnce(&mut LocalState)) -> Result<()> {
    let _guard = LOCAL_LOCK.lock();
    let mut local = read_local(dir);
    f(&mut local);
    atomic_write(&dir.join("local.json"), &serde_json::to_vec_pretty(&local)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../docs/format/fixtures")
            .join(rel)
    }

    fn visible_entries(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn imports_every_valid_fixture_and_lists_them() {
        let data = tempfile::tempdir().unwrap();
        let lib = Library::new(data.path()).unwrap();
        for f in [
            "01-minimal",
            "02-all-features",
            "03-needs-normalising",
            "04-empty",
            "05-from-web",
        ] {
            let (id, m, _) = lib
                .import_file(&fixture(&format!("valid/{f}.tqs")), &mut |_| {})
                .unwrap();
            let opened = lib.open(&id).unwrap();
            assert_eq!(opened.manifest, m);
            assert!(opened.recovered.is_none());
            assert!(opened.dir.join("manifest.json.bak").is_file());
        }
        let shows = lib.list().unwrap();
        assert_eq!(shows.len(), 5);
        let web = shows.iter().find(|s| s.source.is_some()).unwrap();
        assert!(!web.source.as_ref().unwrap().code.is_empty());
    }

    #[test]
    fn legacy_zip_imports() {
        let data = tempfile::tempdir().unwrap();
        let lib = Library::new(data.path()).unwrap();
        let (id, m, warnings) = lib
            .import_file(&fixture("legacy/web-export-STAGE-4821.zip"), &mut |_| {})
            .unwrap();
        assert_eq!(m.show.name, "Old web export");
        assert!(!warnings.is_empty());
        assert_eq!(lib.open(&id).unwrap().manifest.cues.len(), 5);
    }

    #[test]
    fn invalid_fixtures_leave_nothing_in_the_library() {
        let data = tempfile::tempdir().unwrap();
        let lib = Library::new(data.path()).unwrap();
        let invalid = fixture("invalid");
        for e in std::fs::read_dir(&invalid).unwrap().flatten() {
            assert!(
                lib.import_file(&e.path(), &mut |_| {}).is_err(),
                "{:?}",
                e.path()
            );
        }
        assert!(
            visible_entries(lib.root()).is_empty(),
            "{:?}",
            visible_entries(lib.root())
        );
    }

    #[test]
    fn damaged_manifest_recovers_from_backup() {
        let data = tempfile::tempdir().unwrap();
        let lib = Library::new(data.path()).unwrap();
        let (id, m, _) = lib
            .import_file(&fixture("valid/01-minimal.tqs"), &mut |_| {})
            .unwrap();
        lib.open(&id).unwrap(); // makes the .bak
        std::fs::write(
            lib.show_dir(&id).unwrap().join("manifest.json"),
            b"{ half a fil",
        )
        .unwrap();
        let opened = lib.open(&id).unwrap();
        assert_eq!(opened.manifest, m);
        assert!(opened.recovered.unwrap().contains("last good copy"));
        // Opening repaired it, so the Library no longer flags it.
        assert!(!lib.list().unwrap()[0].damaged);
    }

    #[test]
    fn leftover_import_folders_are_removed_on_start() {
        let data = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(data.path().join("library/.tqs-import-abc/audio")).unwrap();
        let lib = Library::new(data.path()).unwrap();
        assert!(visible_entries(lib.root()).is_empty());
    }

    #[test]
    fn show_ids_from_the_webview_are_checked() {
        let data = tempfile::tempdir().unwrap();
        let lib = Library::new(data.path()).unwrap();
        assert!(lib.show_dir("../etc").is_err());
        assert!(lib.show_dir("").is_err());
        assert!(lib.show_dir("0123abcdef").is_ok());
    }

    #[test]
    fn build_from_scratch_export_and_reimport_gives_identical_cues() {
        use crate::editing::{Edit, apply, copy_audio_in};
        let data = tempfile::tempdir().unwrap();
        let lib = Library::new(data.path()).unwrap();
        let id = lib.create_show("Untitled show").unwrap();
        let dir = lib.show_dir(&id).unwrap();
        let mut m = lib.open(&id).unwrap().manifest;
        let a = copy_audio_in(&dir, &fixture("audio/tone-a440.wav")).unwrap();
        let b = copy_audio_in(&dir, &fixture("audio/tone-c262.ogg")).unwrap();
        m = apply(&m, Edit::AddAudio(vec![a, b])).unwrap().0;
        m = apply(&m, Edit::AddControl { silence: true }).unwrap().0;
        let first = m.cues[0].id.clone();
        let patch = serde_json::json!({"volume": 0.75, "fade_in": 0.5, "continue": "after_end", "name": "Opening"});
        m = apply(
            &m,
            Edit::Update {
                id: first.clone(),
                patch: patch.as_object().unwrap().clone(),
            },
        )
        .unwrap()
        .0;
        m = apply(&m, Edit::Duplicate { id: first }).unwrap().0;
        m = apply(&m, Edit::ShowName("Cinderella".into())).unwrap().0;
        atomic_write(&dir.join("manifest.json"), &m.to_json_pretty()).unwrap();

        // Reopen: nothing lost.
        assert_eq!(lib.open(&id).unwrap().manifest, m);

        let out = data.path().join("Cinderella.tqs");
        tqs::write_package(&dir, &out).unwrap();
        let (again, warnings, _) = {
            let (id2, m2, w) = lib.import_file(&out, &mut |_| {}).unwrap();
            (m2, w, id2)
        };
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(again.cues, m.cues, "identical cues after export and import");
        assert_eq!(again.show.name, "Cinderella");
    }

    #[test]
    fn duplicate_show_copies_audio_and_drops_the_web_source() {
        let data = tempfile::tempdir().unwrap();
        let lib = Library::new(data.path()).unwrap();
        let (id, m, _) = lib
            .import_file(&fixture("valid/05-from-web.tqs"), &mut |_| {})
            .unwrap();
        let copy = lib.duplicate_show(&id).unwrap();
        let c = lib.open(&copy).unwrap().manifest;
        assert_eq!(c.show.name, format!("{} copy", m.show.name));
        assert!(c.show.source.is_none());
        assert_eq!(c.cues, m.cues);
        for a in c.assets.values() {
            assert!(lib.show_dir(&copy).unwrap().join(&a.path).is_file());
        }
        assert!(
            visible_entries(lib.root())
                .iter()
                .all(|n| !n.starts_with('.'))
        );
    }

    #[test]
    fn a_hand_corrupted_manifest_opens_from_the_backup_and_says_so() {
        let data = tempfile::tempdir().unwrap();
        let lib = Library::new(data.path()).unwrap();
        let id = lib.create_show("Kept").unwrap();
        lib.open(&id).unwrap(); // refreshes .bak
        std::fs::write(
            lib.show_dir(&id).unwrap().join("manifest.json"),
            "{\"format\": \"thorncue.show\", oops",
        )
        .unwrap();
        let opened = lib.open(&id).unwrap();
        assert_eq!(opened.manifest.show.name, "Kept");
        assert!(opened.recovered.unwrap().contains("\"Kept\" was damaged"));
        // Repaired on disk; the damaged file is kept beside it.
        assert!(lib.open(&id).unwrap().recovered.is_none());
        let dir = lib.show_dir(&id).unwrap();
        assert!(std::fs::read_dir(&dir).unwrap().flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("manifest.json.damaged-")
        }));
    }
}
