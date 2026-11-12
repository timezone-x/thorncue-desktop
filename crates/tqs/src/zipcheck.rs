//! Zip safety (FORMAT.md 1.3). Applied to the central directory before anything is
//! extracted, for `.tqs` packages and for legacy web zips alike.

use std::collections::HashSet;
use std::io::{Read, Seek, Write};

use sha2::{Digest, Sha256};
use zip::{CompressionMethod, ZipArchive};

use crate::error::{Result, TqsError, Warning};
use crate::{Limits, MAX_RATIO, RATIO_MIN_SIZE};

/// One entry that passed the checks and should be looked at.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub index: usize,
    pub name: String,
    /// Declared uncompressed size.
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layout {
    /// `.tqs`: only `manifest.json`, `audio/` and `audio/*` (FORMAT.md 1.1).
    Package,
    /// The web's old Export ZIP: `<CODE>/show.json`, `<CODE>/audio/*`. Any layout passes
    /// here; the importer only reads the files it expects.
    Legacy,
}

pub(crate) fn open<R: Read + Seek>(reader: R) -> Result<ZipArchive<R>> {
    ZipArchive::new(reader).map_err(|_| {
        TqsError::invalid(
            "This file isn't a ThornCue show (it isn't a zip archive, or it is damaged).",
        )
    })
}

/// Rule 1: absolute, `..`, backslash, NUL, empty.
fn check_name(name: &str) -> Result<()> {
    let b = name.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    let bad = name.is_empty()
        || name.contains('\\')
        || name.contains('\0')
        || name.starts_with('/')
        || drive
        || name.split('/').any(|seg| seg == "..");
    if bad {
        return Err(TqsError::invalid(format!(
            "The package contains an unsafe file name: \"{}\".",
            name.escape_debug()
        )));
    }
    Ok(())
}

/// OS junk that is skipped with a warning rather than rejected.
fn is_junk(name: &str) -> bool {
    name.starts_with("__MACOSX/")
        || name.contains("/__MACOSX/")
        || name == ".DS_Store"
        || name.ends_with("/.DS_Store")
        || name == "Thumbs.db"
        || name.ends_with("/Thumbs.db")
}

/// Apply every central-directory rule. Returns the entries to use, and warnings.
pub(crate) fn check<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    limits: &Limits,
    layout: Layout,
) -> Result<(Vec<Entry>, Vec<Warning>)> {
    let count = zip.len();
    if count > limits.max_entries {
        return Err(TqsError::invalid(format!(
            "The package has {count} files; the limit is {}.",
            limits.max_entries
        )));
    }
    let mut warnings = Vec::new();
    let mut seen_lower = HashSet::new();
    let mut total: u64 = 0;
    let mut keep = Vec::new();
    for index in 0..count {
        let f = zip.by_index_raw(index).map_err(|_| {
            TqsError::invalid("The package is damaged (its file list can't be read).")
        })?;
        let name = std::str::from_utf8(f.name_raw())
            .map_err(|_| {
                TqsError::invalid("The package contains a file name that isn't valid UTF-8.")
            })?
            .to_string();
        check_name(&name)?;
        if f.encrypted() {
            return Err(TqsError::invalid(format!(
                "{name} is encrypted. ThornCue packages can't be encrypted."
            )));
        }
        // Unix mode bits live in the top half of the external attributes, whichever system
        // the zip claims to come from (as tqs_ref.py reads them).
        let unix_mode = f.external_attributes() >> 16;
        if f.is_symlink() || unix_mode & 0o170000 == 0o120000 {
            return Err(TqsError::invalid(format!(
                "{name} is a symbolic link, which isn't allowed."
            )));
        }
        if !matches!(
            f.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(TqsError::invalid(format!(
                "{name} uses an unsupported compression method."
            )));
        }
        if !seen_lower.insert(name.to_lowercase()) {
            return Err(TqsError::invalid(format!(
                "The package has two files called \"{name}\" (ignoring upper and lower case)."
            )));
        }
        if is_junk(&name) {
            warnings.push(Warning(format!("Skipping {name}.")));
            continue;
        }
        if name.ends_with('/') {
            if layout == Layout::Package && name != "audio/" {
                return Err(TqsError::invalid(format!(
                    "Unexpected folder \"{name}\" in the package."
                )));
            }
            continue;
        }
        if layout == Layout::Package && name != "manifest.json" && !name.starts_with("audio/") {
            return Err(TqsError::invalid(format!(
                "Unexpected file \"{name}\" in the package."
            )));
        }
        let size = f.size();
        if size > RATIO_MIN_SIZE && size > MAX_RATIO * f.compressed_size().max(1) {
            return Err(TqsError::invalid(format!(
                "{name} is compressed suspiciously well; refusing it as a possible zip bomb."
            )));
        }
        total = total.saturating_add(size);
        keep.push(Entry { index, name, size });
    }
    if total > limits.max_total {
        return Err(too_big(total, limits.max_total));
    }
    Ok((keep, warnings))
}

fn gib(n: u64) -> f64 {
    n as f64 / (1024.0 * 1024.0 * 1024.0)
}

pub(crate) fn too_big(total: u64, max: u64) -> TqsError {
    TqsError::invalid(format!(
        "The package unpacks to {:.1} GiB, over the {:.1} GiB limit.",
        gib(total),
        gib(max)
    ))
}

/// Counts bytes across every entry of one import (rule 6: stop the moment the real count
/// passes the limit, because zip bombs lie about sizes).
pub(crate) struct Budget {
    pub used: u64,
    pub max: u64,
}

/// Copy one entry, refusing to go past its declared size or the import's budget.
/// Returns (bytes, sha256 hex). `on_bytes` is called with each chunk's length.
pub(crate) fn copy_counted(
    src: &mut dyn Read,
    dst: &mut dyn Write,
    limit: u64,
    name: &str,
    budget: &mut Budget,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut n: u64 = 0;
    loop {
        let got = match src.read(&mut buf) {
            Ok(0) => break,
            Ok(got) => got,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                return Err(TqsError::invalid(format!(
                    "{name} is damaged (it can't be unpacked)."
                )));
            }
        };
        n += got as u64;
        budget.used += got as u64;
        if n > limit {
            return Err(TqsError::invalid(format!(
                "{name} is larger than the package says it is."
            )));
        }
        if budget.used > budget.max {
            return Err(too_big(budget.used, budget.max));
        }
        hasher.update(&buf[..got]);
        dst.write_all(&buf[..got]).map_err(|e| TqsError::Io {
            action: "write",
            path: name.into(),
            source: e,
        })?;
        on_bytes(got as u64);
    }
    Ok((n, hex(&hasher.finalize())))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_names_are_refused() {
        for bad in ["/x", "C:/x", "c:\\x", "a/../b", "..", "a\\b", "a\0b", ""] {
            assert!(check_name(bad).is_err(), "{bad:?} should be refused");
        }
        for ok in [
            "manifest.json",
            "audio/a.wav",
            "audio/",
            "a..b/c",
            "STAGE-1/show.json",
        ] {
            assert!(check_name(ok).is_ok(), "{ok:?} should pass");
        }
    }

    #[test]
    fn junk_is_recognised() {
        assert!(is_junk("__MACOSX/audio/._t.wav"));
        assert!(is_junk(".DS_Store"));
        assert!(is_junk("CODE/Thumbs.db"));
        assert!(!is_junk("audio/thumbs.wav"));
    }
}
