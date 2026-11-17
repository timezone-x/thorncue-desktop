//! Writing a package (FORMAT.md section 3).

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

use crate::error::{Result, TqsError};
use crate::manifest::{Generator, Kind, Manifest};
use crate::read::Progress;
use crate::time::now_rfc3339;
use crate::zipcheck::hex;

/// Zip a show folder (an unpacked `.tqs`: `manifest.json` + `audio/`) into `out_path`.
///
/// Audio files that are missing on disk are dropped and their cues get `asset: null`, so one
/// lost file doesn't stop the export (3.1). Files are hashed in a first pass so the manifest
/// can go first (3.2, 3.3). The package is written to a temp file and renamed into place.
pub fn write_package(show_dir: &Path, out_path: &Path) -> Result<Manifest> {
    write_package_with_progress(show_dir, out_path, &mut |_| {})
}

pub fn write_package_with_progress(
    show_dir: &Path,
    out_path: &Path,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<Manifest> {
    let mpath = show_dir.join("manifest.json");
    let bytes = std::fs::read(&mpath).map_err(TqsError::io("read", &mpath))?;
    let (manifest, _) = Manifest::from_slice(&bytes)?;
    write_manifest_package(show_dir, manifest, out_path, on_progress)
}

/// What goes into a `.tqc` or `.tqg` (FORMAT.md 6).
#[derive(Debug, Clone)]
pub enum Piece {
    /// These cues (any order; the file keeps show order) as a `.tqc`: no groups.
    Cues(Vec<String>),
    /// One group and all its cues as a `.tqg`.
    Group(String),
}

/// Write part of a show folder as a `.tqc` or `.tqg` (FORMAT.md 6.1-6.3): the picked cues in
/// show order, only the assets they use, and for a group exactly that group. Same writer as a
/// show, so the same checks, temp file and rename apply.
pub fn write_piece(show_dir: &Path, out_path: &Path, piece: &Piece) -> Result<Manifest> {
    let mpath = show_dir.join("manifest.json");
    let bytes = std::fs::read(&mpath).map_err(TqsError::io("read", &mpath))?;
    let (mut manifest, _) = Manifest::from_slice(&bytes)?;
    match piece {
        Piece::Cues(ids) => {
            manifest.kind = Kind::Cues;
            manifest.cues.retain(|c| ids.contains(&c.id));
            for c in &mut manifest.cues {
                c.group = None;
            }
            manifest.groups.clear();
            if manifest.cues.is_empty() {
                return Err(TqsError::invalid("Those cues are no longer in the show."));
            }
        }
        Piece::Group(id) => {
            manifest.kind = Kind::Group;
            manifest
                .cues
                .retain(|c| c.group.as_deref() == Some(id.as_str()));
            manifest.groups.retain(|g| &g.id == id);
            if manifest.cues.is_empty() || manifest.groups.len() != 1 {
                return Err(TqsError::invalid("That group is no longer in the show."));
            }
        }
    }
    let used: std::collections::HashSet<String> = manifest
        .cues
        .iter()
        .filter_map(|c| c.asset.clone())
        .collect();
    manifest.assets.retain(|id, _| used.contains(id));
    write_manifest_package(show_dir, manifest, out_path, &mut |_| {})
}

fn write_manifest_package(
    show_dir: &Path,
    mut manifest: Manifest,
    out_path: &Path,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<Manifest> {
    // Pass 1: hash every file; drop the ones that aren't there.
    let mut missing = Vec::new();
    for (id, asset) in manifest.assets.iter_mut() {
        let p = show_dir.join(&asset.path);
        match hash_file(&p) {
            Ok((sha, n)) if n > 0 => {
                asset.sha256 = sha;
                asset.bytes = n;
            }
            Ok(_) => missing.push(id.clone()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => missing.push(id.clone()),
            Err(e) => {
                return Err(TqsError::Io {
                    action: "read",
                    path: p,
                    source: e,
                });
            }
        }
    }
    for id in &missing {
        manifest.assets.shift_remove(id);
    }
    for cue in &mut manifest.cues {
        if cue.asset.as_ref().is_some_and(|a| missing.contains(a)) {
            cue.asset = None;
        }
    }
    // Groups listed in the order they first appear (FORMAT.md 2.7, "writers SHOULD").
    let first = |id: &str| {
        manifest
            .cues
            .iter()
            .position(|c| c.group.as_deref() == Some(id))
    };
    let mut order: Vec<(Option<usize>, crate::manifest::Group)> =
        manifest.groups.drain(..).map(|g| (None, g)).collect();
    for (pos, g) in &mut order {
        *pos = first(&g.id);
    }
    order.sort_by_key(|(pos, _)| *pos);
    manifest.groups = order.into_iter().map(|(_, g)| g).collect();

    manifest.generator = Generator {
        name: "thorncue-desktop".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    manifest.exported_at = now_rfc3339();

    let out_dir = out_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let tmp = tempfile::Builder::new()
        .prefix(".thorncue-export-")
        .suffix(".part")
        .tempfile_in(out_dir)
        .map_err(TqsError::io("create a temporary file in", out_dir))?;
    let file = tmp.reopen().map_err(TqsError::io("write", tmp.path()))?;
    let mut zip = ZipWriter::new(BufWriter::new(file));
    let err = |e: zip::result::ZipError| TqsError::Io {
        action: "write",
        path: out_path.to_path_buf(),
        source: std::io::Error::other(e),
    };
    let ioerr = |e: std::io::Error| TqsError::Io {
        action: "write",
        path: out_path.to_path_buf(),
        source: e,
    };

    zip.start_file(
        "manifest.json",
        SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
    )
    .map_err(err)?;
    zip.write_all(&manifest.to_json_pretty()).map_err(ioerr)?;

    // Pass 2: audio, STORE. Re-hash while copying so a file that changed between the passes
    // can't produce a package whose manifest lies.
    let total = manifest.audio_bytes();
    let mut done = 0u64;
    on_progress(Progress { done, total });
    let mut buf = vec![0u8; 256 * 1024];
    for asset in manifest.assets.values() {
        let p = show_dir.join(&asset.path);
        let opts = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .large_file(asset.bytes >= u32::MAX as u64);
        zip.start_file(asset.path.as_str(), opts).map_err(err)?;
        let mut src = BufReader::new(File::open(&p).map_err(TqsError::io("read", &p))?);
        let mut hasher = Sha256::new();
        let mut n = 0u64;
        loop {
            let got = src.read(&mut buf).map_err(TqsError::io("read", &p))?;
            if got == 0 {
                break;
            }
            hasher.update(&buf[..got]);
            zip.write_all(&buf[..got]).map_err(ioerr)?;
            n += got as u64;
            done += got as u64;
            on_progress(Progress { done, total });
        }
        if n != asset.bytes || hex(&hasher.finalize()) != asset.sha256 {
            return Err(TqsError::invalid(format!(
                "{} changed while it was being exported. Try the export again.",
                asset.path
            )));
        }
    }
    let mut inner = zip.finish().map_err(err)?;
    inner.flush().map_err(ioerr)?;
    let file = inner.into_inner().map_err(|e| ioerr(e.into_error()))?;
    file.sync_all().map_err(ioerr)?;
    drop(file);
    tmp.persist(out_path).map_err(|e| ioerr(e.error))?;
    // The temp file was private (0600); an exported show is meant to be passed on.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(out_path, std::fs::Permissions::from_mode(0o644));
    }
    Ok(manifest)
}

fn hash_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut f = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut n = 0u64;
    loop {
        let got = f.read(&mut buf)?;
        if got == 0 {
            break;
        }
        hasher.update(&buf[..got]);
        n += got as u64;
    }
    Ok((hex(&hasher.finalize()), n))
}
