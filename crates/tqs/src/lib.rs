//! ThornCue packages (`.tqs` shows, `.tqc` cue files, `.tqg` group files), format version 1,
//! spec 1.1. `docs/format/FORMAT.md` is the spec;
//! this is a port of `docs/format/tools/tqs_ref.py`, following the spec where they differ.
//!
//! No Tauri and no kira here (CLAUDE.md rule 6).

mod audio;
mod error;
mod legacy;
mod manifest;
mod read;
mod time;
mod write;
mod zipcheck;

pub use audio::check_audio_file;
pub use error::{Result, TqsError, Warning};
pub use legacy::import_legacy_zip;
pub use manifest::{
    AUDIO_EXTS, Asset, Continue, Cue, CueType, DEFAULT_GROUP_COLOR, FORMAT, FORMAT_VERSION,
    Generator, Group, Kind, MAX_GROUPS, Manifest, Show, Source, gather_groups, normalise_cuts,
    normalise_manifest, normalise_manifest_of,
};
pub use read::{
    PackageKind, Progress, detect_kind, read_package, read_package_of, read_package_with_progress,
};
pub use time::now_rfc3339;
pub use write::{write_package, write_package_with_progress};

/// Reader limits (FORMAT.md 1.3). The desktop default for the total is 4 GiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Most bytes a package may unpack to.
    pub max_total: u64,
    /// Most entries in the zip.
    pub max_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_total: 4 * 1024 * 1024 * 1024,
            max_entries: MAX_ENTRIES,
        }
    }
}

pub const MAX_ENTRIES: usize = 2000;
pub const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_RATIO: u64 = 200;
pub const RATIO_MIN_SIZE: u64 = 1024 * 1024;
pub const MAX_CUES: usize = 2000;
pub const MAX_CUTS: usize = 50;
