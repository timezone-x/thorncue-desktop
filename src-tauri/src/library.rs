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
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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
            let name = e.file_name();
            if name.to_string_lossy().starts_with(".tqs-import-") {
                tracing::info!("removing an unfinished import: {}", e.path().display());
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
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
        })
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
    atomic_write(&dir.join("local.json"), &serde_json::to_vec_pretty(local)?)
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
        assert!(lib.list().unwrap()[0].damaged);
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
}
