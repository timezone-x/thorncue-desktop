//! Reading a package (FORMAT.md section 4).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;

use crate::audio::check_audio_file;
use crate::error::{Result, TqsError, Warning};
use crate::manifest::Manifest;
use crate::zipcheck::{self, Budget, Layout};
use crate::{Limits, MAX_MANIFEST_BYTES};

/// Audio bytes unpacked so far, out of the total, for a progress bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

/// Validate a `.tqs` and unpack it into `dest_dir`, which must not exist yet.
///
/// Everything is extracted into a temporary folder next to `dest_dir` and only renamed into
/// place once every check has passed, so a rejected package leaves nothing behind.
pub fn read_package(
    path: &Path,
    dest_dir: &Path,
    limits: Limits,
) -> Result<(Manifest, Vec<Warning>)> {
    read_package_with_progress(path, dest_dir, limits, &mut |_| {})
}

pub fn read_package_with_progress(
    path: &Path,
    dest_dir: &Path,
    limits: Limits,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<(Manifest, Vec<Warning>)> {
    let file = File::open(path).map_err(TqsError::io("open", path))?;
    let mut zip = zipcheck::open(BufReader::new(file))?;
    let (entries, mut warnings) = zipcheck::check(&mut zip, &limits, Layout::Package)?;
    let by_name: HashMap<&str, &zipcheck::Entry> =
        entries.iter().map(|e| (e.name.as_str(), e)).collect();

    let Some(minfo) = by_name.get("manifest.json") else {
        return Err(TqsError::invalid(
            "This file isn't a ThornCue show (it has no manifest.json).",
        ));
    };
    if entries.first().map(|e| e.name.as_str()) != Some("manifest.json") {
        warnings.push(Warning(
            "manifest.json isn't the first file in the package (writers should put it first)."
                .into(),
        ));
    }
    if minfo.size > MAX_MANIFEST_BYTES {
        return Err(TqsError::invalid("manifest.json is too large."));
    }
    let mut data = Vec::new();
    zip.by_index(minfo.index)
        .map_err(|_| TqsError::invalid("manifest.json is damaged."))?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|_| TqsError::invalid("manifest.json is damaged."))?;
    if data.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(TqsError::invalid("manifest.json is too large."));
    }
    let (manifest, w2) = Manifest::from_slice(&data)?;
    warnings.extend(w2);

    // Cross-references (4.3).
    let wanted: HashMap<&str, &str> = manifest
        .assets
        .iter()
        .map(|(id, a)| (a.path.as_str(), id.as_str()))
        .collect();
    for (id, a) in &manifest.assets {
        if !by_name.contains_key(a.path.as_str()) {
            return Err(TqsError::invalid(format!(
                "Audio file {} (audio {id}) is listed but missing from the package.",
                a.path
            )));
        }
    }
    for e in &entries {
        if e.name.starts_with("audio/") && !wanted.contains_key(e.name.as_str()) {
            warnings.push(Warning(format!("Skipping {}: no cue uses it.", e.name)));
        }
    }

    let tmp = make_temp_dir(dest_dir)?;
    let audio_dir = tmp.path().join("audio");
    std::fs::create_dir(&audio_dir).map_err(TqsError::io("create", &audio_dir))?;

    let total: u64 = manifest.audio_bytes();
    let mut done = 0u64;
    on_progress(Progress { done, total });
    let mut budget = Budget {
        used: 0,
        max: limits.max_total,
    };
    for (id, asset) in &manifest.assets {
        let entry = by_name[asset.path.as_str()];
        let out_path = tmp.path().join(&asset.path);
        let mut out = File::create(&out_path).map_err(TqsError::io("create", &out_path))?;
        let mut src = zip.by_index(entry.index).map_err(|_| {
            TqsError::invalid(format!("{} is damaged (it can't be unpacked).", asset.path))
        })?;
        let (n, digest) = zipcheck::copy_counted(
            &mut src,
            &mut out,
            entry.size.min(asset.bytes),
            &asset.path,
            &mut budget,
            &mut |chunk| {
                done += chunk;
                on_progress(Progress { done, total });
            },
        )?;
        drop(src);
        if n != asset.bytes || n != entry.size {
            return Err(TqsError::invalid(format!(
                "{} is damaged (wrong size). Export the show again.",
                asset.path
            )));
        }
        if digest != asset.sha256 {
            return Err(TqsError::invalid(format!(
                "{} is damaged (its checksum doesn't match). Export the show again.",
                asset.path
            )));
        }
        out.sync_all().map_err(TqsError::io("write", &out_path))?;
        drop(out);
        if check_audio_file(&out_path).is_err() {
            return Err(TqsError::invalid(format!(
                "{} (audio {id}) isn't audio ThornCue can play. {}",
                asset.path,
                crate::audio::UNPLAYABLE_HINT
            )));
        }
    }

    let mpath = tmp.path().join("manifest.json");
    write_synced(&mpath, &manifest.to_json_pretty())?;
    move_into_place(tmp, dest_dir)?;
    Ok((manifest, warnings))
}

/// A temp folder next to `dest_dir` (same filesystem, so the final rename is atomic).
pub(crate) fn make_temp_dir(dest_dir: &Path) -> Result<tempfile::TempDir> {
    let parent = dest_dir
        .parent()
        .ok_or_else(|| TqsError::invalid("The destination folder has no parent folder."))?;
    std::fs::create_dir_all(parent).map_err(TqsError::io("create", parent))?;
    tempfile::Builder::new()
        .prefix(".tqs-import-")
        .tempdir_in(parent)
        .map_err(TqsError::io("create a temporary folder in", parent))
}

pub(crate) fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = File::create(path).map_err(TqsError::io("create", path))?;
    f.write_all(bytes).map_err(TqsError::io("write", path))?;
    f.sync_all().map_err(TqsError::io("write", path))
}

/// Rename the finished temp folder to `dest_dir`. On failure the temp folder is removed.
pub(crate) fn move_into_place(tmp: tempfile::TempDir, dest_dir: &Path) -> Result<()> {
    if dest_dir.exists() {
        return Err(TqsError::invalid(format!(
            "{} already exists.",
            dest_dir.display()
        )));
    }
    std::fs::rename(tmp.path(), dest_dir).map_err(TqsError::io("move the show into", dest_dir))?;
    // The folder now lives at dest_dir; don't let TempDir delete it.
    let _ = tmp.keep();
    #[cfg(unix)]
    if let Some(parent) = dest_dir.parent()
        && let Ok(d) = File::open(parent)
    {
        let _ = d.sync_all();
    }
    Ok(())
}
