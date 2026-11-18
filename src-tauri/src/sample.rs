//! The sample show (thorncue-spec#11): a short show that teaches ThornCue through its cue
//! names, offered on first run and from Settings. Same behaviour as the web (thorncue-web
//! README "Sample show", `utils.open_sample_show` and `reset_sample_show`), minus the web's
//! shared audio: one user, so each open is a plain `.tqs` import.
//!
//! The sample is an ordinary show in the library. It's tracked by its library id
//! (`Settings::sample_show_id`), not its name, so a renamed sample is still the sample.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::library::{Library, LocalState};
use crate::settings::Settings;

/// `sample/sample-show.tqs`, byte for byte from thorncue-spec `samples/` (see sample/README.md).
pub const SAMPLE_TQS: &[u8] = include_bytes!("../sample/sample-show.tqs");
pub const SAMPLE_NAME: &str = "Sample show";

/// Open and Reset one at a time, so two quick clicks still make one show.
static SAMPLE_LOCK: Mutex<()> = Mutex::new(());

/// The bundled package as a file, for the normal import path. Removed when dropped.
fn bundled_file(tmp_dir: &Path) -> Result<tempfile::NamedTempFile> {
    std::fs::create_dir_all(tmp_dir)?;
    let mut f = tempfile::Builder::new()
        .prefix("sample-")
        .suffix(".tqs")
        .tempfile_in(tmp_dir)
        .context("creating a temp file for the sample show")?;
    std::io::Write::write_all(&mut f, SAMPLE_TQS)?;
    Ok(f)
}

/// The sample show still in the library, if there is one.
fn existing(lib: &Library, settings: &Settings) -> Option<String> {
    let id = settings.sample_show_id.as_deref()?;
    lib.show_dir(id)
        .ok()
        .filter(|d| d.join("manifest.json").is_file() || d.join("manifest.json.bak").is_file())
        .map(|_| id.to_string())
}

fn import_fresh(lib: &Library, tmp_dir: &Path) -> Result<String> {
    let file = bundled_file(tmp_dir)?;
    let (id, m, warnings) = lib
        .import_file(file.path(), &mut |_| {})
        .context("importing the bundled sample show")?;
    if !warnings.is_empty() {
        tracing::warn!("the sample show imported with warnings: {warnings:?}");
    }
    debug_assert_eq!(m.show.name, SAMPLE_NAME);
    Ok(id)
}

/// `(id, created)`: the sample show, imported fresh if it isn't in the library (never opened,
/// or deleted). Also answers the welcome card. Updates `settings`; the caller saves them.
pub fn open(lib: &Library, settings: &mut Settings, tmp_dir: &Path) -> Result<(String, bool)> {
    let _guard = SAMPLE_LOCK.lock();
    settings.welcome_seen = true;
    if let Some(id) = existing(lib, settings) {
        return Ok((id, false));
    }
    let id = import_fresh(lib, tmp_dir)?;
    settings.sample_show_id = Some(id.clone());
    Ok((id, true))
}

/// Replace the sample show with a fresh copy, keeping its id (as the web keeps its code), or
/// import one if there's none. The old copy goes to the trash. Updates `settings`.
pub fn reset(lib: &Library, settings: &mut Settings, tmp_dir: &Path) -> Result<String> {
    let _guard = SAMPLE_LOCK.lock();
    settings.welcome_seen = true;
    if let Some(id) = existing(lib, settings) {
        let file = bundled_file(tmp_dir)?;
        lib.replace_with_file(&id, file.path(), &mut |_| {}, |old, _| LocalState {
            // A fresh copy: standby at the top, nothing collapsed. When it was last used stays.
            last_opened: old.last_opened,
            imported_at: Some(tqs::now_rfc3339()),
            ..Default::default()
        })
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("replacing the sample show")?;
        return Ok(id);
    }
    let id = import_fresh(lib, tmp_dir)?;
    settings.sample_show_id = Some(id.clone());
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// The cue names are the teaching text: they must be the spec file's, unchanged
    /// (thorncue-spec samples/README.md).
    const SPEC_CUE_NAMES: [&str; 9] = [
        "Press Space to GO. The next cue is now on standby (the ▶ in the Q column)",
        "This cue fades in and out — press \\ to open the inspector and see the times",
        "Trimmed, with a cut: see the markers and the cut on the waveform in the inspector",
        "Loops until you press Esc (Stop all)",
        "Auto-continues into the next cue when it ends",
        "Silence: stops everything, waits 2 seconds, then auto-continues",
        "Started on its own by the silence before it. GO the next cue to fade it out",
        "Fade-out cue: fades everything that's playing over 3 seconds",
        "The end. Rename, edit or delete this show; Settings can reset it",
    ];

    fn setup() -> (tempfile::TempDir, Library, Settings) {
        let data = tempfile::tempdir().unwrap();
        let lib = Library::new(data.path()).unwrap();
        (data, lib, Settings::default())
    }

    #[test]
    fn the_bundled_sample_is_the_spec_file() {
        let readme = include_str!("../sample/README.md");
        let recorded = readme
            .lines()
            .find_map(|l| l.strip_prefix("sha256: "))
            .expect("sample/README.md records the hash");
        let actual: String = Sha256::digest(SAMPLE_TQS)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(actual, recorded.trim());
        assert_eq!(
            actual,
            "d0f8cb329f09cde1cace187ea9c6b871be4e61d4984292311572ba6dd1d5f55a"
        );
    }

    #[test]
    fn the_imported_show_is_the_spec_show() {
        let (data, lib, mut s) = setup();
        let (id, created) = open(&lib, &mut s, &data.path().join("tmp")).unwrap();
        assert!(created);
        let m = lib.open(&id).unwrap().manifest;
        assert_eq!(m.show.name, SAMPLE_NAME);
        let names: Vec<&str> = m.cues.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, SPEC_CUE_NAMES);
        assert_eq!(m.groups.len(), 2, "two groups, kept through import");
        assert_eq!(m.assets.len(), 3);
        // The temp copy of the package is gone.
        assert_eq!(
            std::fs::read_dir(data.path().join("tmp")).unwrap().count(),
            0
        );
    }

    #[test]
    fn the_welcome_answer_is_remembered() {
        let (data, lib, mut s) = setup();
        assert!(!s.welcome_seen, "a fresh install shows the welcome card");
        open(&lib, &mut s, &data.path().join("tmp")).unwrap();
        let p = data.path().join("settings.json");
        s.save(&p).unwrap();
        let back = Settings::load(&p);
        assert!(back.welcome_seen);
        assert_eq!(back.sample_show_id, s.sample_show_id);
        assert_eq!(back.for_ui().sample_show_id, s.sample_show_id);
    }

    #[test]
    fn opening_twice_gives_one_show_even_renamed() {
        let (data, lib, mut s) = setup();
        let tmp = data.path().join("tmp");
        let (a, _) = open(&lib, &mut s, &tmp).unwrap();
        let dir = lib.show_dir(&a).unwrap();
        let mut m = lib.open(&a).unwrap().manifest;
        m.show.name = "My practice show".into();
        crate::fsutil::atomic_write(&dir.join("manifest.json"), &m.to_json_pretty()).unwrap();
        let (b, created) = open(&lib, &mut s, &tmp).unwrap();
        assert_eq!(a, b);
        assert!(!created);
        assert_eq!(lib.list().unwrap().len(), 1);
    }

    #[test]
    fn opening_after_delete_imports_a_new_one() {
        let (data, lib, mut s) = setup();
        let tmp = data.path().join("tmp");
        let (a, _) = open(&lib, &mut s, &tmp).unwrap();
        // Deleting moves it to the OS trash; here it just goes.
        std::fs::remove_dir_all(lib.show_dir(&a).unwrap()).unwrap();
        let (b, created) = open(&lib, &mut s, &tmp).unwrap();
        assert!(created);
        assert_ne!(a, b);
        assert_eq!(s.sample_show_id.as_deref(), Some(b.as_str()));
        assert_eq!(lib.list().unwrap().len(), 1);
    }

    #[test]
    fn reset_replaces_it_in_place() {
        let (data, lib, mut s) = setup();
        let tmp = data.path().join("tmp");
        let (id, _) = open(&lib, &mut s, &tmp).unwrap();
        let dir = lib.show_dir(&id).unwrap();
        let mut m = lib.open(&id).unwrap().manifest;
        m.show.name = "Edited".into();
        m.cues.truncate(2);
        crate::fsutil::atomic_write(&dir.join("manifest.json"), &m.to_json_pretty()).unwrap();
        crate::library::update_local(&dir, |l| l.standby_index = 2).unwrap();

        let again = reset(&lib, &mut s, &tmp).unwrap();
        assert_eq!(again, id, "same show, fresh content");
        let fresh = lib.open(&id).unwrap();
        assert_eq!(fresh.manifest.show.name, SAMPLE_NAME);
        assert_eq!(fresh.manifest.cues.len(), 9);
        assert_eq!(fresh.local.standby_index, 0);
        assert_eq!(lib.list().unwrap().len(), 1);
    }

    #[test]
    fn reset_with_no_sample_imports_one() {
        let (data, lib, mut s) = setup();
        let id = reset(&lib, &mut s, &data.path().join("tmp")).unwrap();
        assert_eq!(s.sample_show_id.as_deref(), Some(id.as_str()));
        assert!(s.welcome_seen);
        assert_eq!(lib.list().unwrap().len(), 1);
    }
}
