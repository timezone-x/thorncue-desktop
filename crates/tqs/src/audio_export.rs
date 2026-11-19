//! Audio export (FORMAT.md section 7): cue numbers, file names, the `<show> audio.zip` with its
//! `cue-list.csv`, and recognising one when it's imported (7.5). Rendering is the engine's
//! (`engine::cue_samples`); this module only names, lists and zips what the app hands it.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

use crate::error::{Result, TqsError};
use crate::manifest::{Continue, CueType, Manifest};
use crate::{Limits, zipcheck};

/// The zip comment that marks an audio export (7.3, 7.5).
pub const AUDIO_EXPORT_COMMENT: &str = "thorncue.audio-export";
pub const CUE_LIST: &str = "cue-list.csv";
pub const CSV_HEADER: &str =
    "number,name,type,file,group,volume %,fade in,fade out,pre-wait,loop,auto-continue,length";

/// 7.5: an audio export with no `.tqs` inside.
pub const AUDIO_EXPORT_REFUSED: &str =
    "This is an audio export, not a show. Export it again with \"Include the .tqs\" ticked.";
/// 7.5: an audio export whose `.tqs` was imported.
pub const AUDIO_EXPORT_IMPORTED: &str =
    "This is an audio export. Imported the show file inside it.";
/// 7.5: an audio export offered where cues (.tqc/.tqg) are expected.
pub const AUDIO_EXPORT_FOR_CUES: &str = "This is an audio export, not a cue file. If it has a .tqs inside, import it as a show from the Library (Import file…).";

// ---------------------------------------------------------------------------
// Cue numbers and names (7.3)
// ---------------------------------------------------------------------------

/// The cue at `index` (from 0) in a show of `count` cues: "Q3", or with `pad` zero-padded to
/// the width of `count` ("Q03" in a show of 24). The only place in the Rust code that builds
/// a cue number (the UI's is `cueNumber` in src/lib/format.ts). Custom cue numbers
/// (thorncue-spec#16) will replace it.
pub fn cue_number(index: usize, count: usize, pad: bool) -> String {
    let width = if pad {
        count.max(1).to_string().len()
    } else {
        1
    };
    format!("Q{:0width$}", index + 1)
}

/// FORMAT.md 7.3's file-name rule: drop unsafe characters, collapse whitespace, trim spaces
/// and dots, cut to `limit` characters, fall back if empty, and dodge Windows' reserved names.
pub fn safe_name(name: &str, fallback: &str, limit: usize) -> String {
    let kept: String = name
        .chars()
        .filter(|&c| {
            !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                && !matches!(c, '\u{0}'..='\u{8}' | '\u{e}'..='\u{1f}' | '\u{7f}')
        })
        .collect();
    let collapsed = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    let trim = |s: &str| s.trim_matches(|c| c == ' ' || c == '.').to_string();
    let mut s = trim(&collapsed);
    s = trim(&s.chars().take(limit).collect::<String>());
    if s.is_empty() {
        s = fallback.to_string();
    }
    if is_reserved(&s) {
        s.push('_');
    }
    s
}

fn is_reserved(s: &str) -> bool {
    let stem = s.split('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || ((stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.len() == 4
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

/// Names claimed per folder, compared case-insensitively; later claimants get " (2)", " (3)"...
#[derive(Debug, Default)]
pub struct Namer {
    taken: HashMap<String, HashSet<String>>,
}

impl Namer {
    pub fn claim(&mut self, folder: &str, stem: &str, ext: &str) -> String {
        let used = self.taken.entry(folder.to_lowercase()).or_default();
        let mut n = 1;
        let mut name = format!("{stem}{ext}");
        while used.contains(&name.to_lowercase()) {
            n += 1;
            name = format!("{stem} ({n}){ext}");
        }
        used.insert(name.to_lowercase());
        name
    }
}

// ---------------------------------------------------------------------------
// Options and what gets exported
// ---------------------------------------------------------------------------

/// Which cues (7.3 "Cues").
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum CueChoice {
    All,
    Selected(Vec<String>),
    Group(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioFormat {
    Wav,
    Mp3,
}

impl AudioFormat {
    pub fn ext(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
        }
    }
}

/// The Export audio options (7.3), defaults as the spec's.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct AudioExportOptions {
    pub cues: CueChoice,
    /// Render each cue (7.1); off: the original files byte for byte.
    pub edits: bool,
    /// With edits on.
    pub format: AudioFormat,
    pub numbers: bool,
    pub folders: bool,
    pub cue_list: bool,
    pub tqs: bool,
}

impl Default for AudioExportOptions {
    fn default() -> Self {
        Self {
            cues: CueChoice::All,
            edits: true,
            format: AudioFormat::Wav,
            numbers: true,
            folders: false,
            cue_list: true,
            tqs: false,
        }
    }
}

/// The picked cues' indexes, in show order.
pub fn picked(m: &Manifest, cues: &CueChoice) -> Vec<usize> {
    (0..m.cues.len())
        .filter(|&i| match cues {
            CueChoice::All => true,
            CueChoice::Selected(ids) => ids.contains(&m.cues[i].id),
            CueChoice::Group(g) => m.cues[i].group.as_deref() == Some(g.as_str()),
        })
        .collect()
}

/// `<show> audio.zip`, or `<show> - Q4 to Q9 audio.zip` / `<show> - Storm audio.zip` for part.
pub fn zip_name(m: &Manifest, cues: &CueChoice, picked: &[usize]) -> String {
    let n = m.cues.len();
    let show = safe_name(&m.show.name, "Show", 100);
    let part = match cues {
        CueChoice::Selected(_) => match (picked.first(), picked.last()) {
            (Some(&a), Some(&b)) if a == b => format!(" - {}", cue_number(a, n, false)),
            (Some(&a), Some(&b)) => format!(
                " - {} to {}",
                cue_number(a, n, false),
                cue_number(b, n, false)
            ),
            _ => String::new(),
        },
        CueChoice::Group(g) => m
            .groups
            .iter()
            .find(|x| &x.id == g)
            .map(|x| format!(" - {}", x.name))
            .unwrap_or_default(),
        CueChoice::All => String::new(),
    };
    format!(
        "{}.zip",
        safe_name(&format!("{show}{part} audio"), "Show audio", 100)
    )
}

/// The inspector's single-cue export: "<show> Q3 <cue>.<ext>".
pub fn single_file_name(m: &Manifest, index: usize, ext: &str) -> String {
    let q = cue_number(index, m.cues.len(), false);
    let name = m.cues.get(index).map(|c| c.name.as_str()).unwrap_or("");
    format!(
        "{}.{ext}",
        safe_name(&format!("{} {q} {name}", m.show.name), &q, 100)
    )
}

/// Paths in the zip for the cues that have a file (7.3 Names, Folders, Collisions).
/// `has_file` gives each picked cue's extension (lowercase, no dot), or `None` for no file.
/// Returns (index -> path, the `.tqs` entry name).
pub fn plan_paths(
    m: &Manifest,
    o: &AudioExportOptions,
    picked: &[usize],
    has_file: &dyn Fn(usize) -> Option<String>,
) -> (HashMap<usize, String>, String) {
    let n = m.cues.len();
    let mut namer = Namer::default();
    let tqs_name = format!("{}.tqs", safe_name(&m.show.name, "Show", 100));
    if o.cue_list {
        namer.claim("", CUE_LIST, "");
    }
    if o.tqs {
        namer.claim("", &tqs_name, "");
    }
    let mut folders: HashMap<&str, String> = HashMap::new();
    let mut paths = HashMap::new();
    for &i in picked {
        let Some(ext) = has_file(i) else { continue };
        let c = &m.cues[i];
        let group = c
            .group
            .as_deref()
            .and_then(|g| m.groups.iter().find(|x| x.id == g));
        let folder = match group {
            Some(g) if o.folders => folders
                .entry(g.id.as_str())
                .or_insert_with(|| namer.claim("", &safe_name(&g.name, "Group", 60), ""))
                .clone(),
            _ => String::new(),
        };
        let number = cue_number(i, n, true);
        let stem = if o.numbers {
            safe_name(&format!("{number} {}", c.name), &number, 100)
        } else {
            safe_name(&c.name, &number, 100)
        };
        let name = namer.claim(&folder, &stem, &format!(".{ext}"));
        paths.insert(
            i,
            if folder.is_empty() {
                name
            } else {
                format!("{folder}/{name}")
            },
        );
    }
    (paths, tqs_name)
}

// ---------------------------------------------------------------------------
// cue-list.csv (7.4)
// ---------------------------------------------------------------------------

/// Seconds with up to 3 decimals and no trailing zeros.
pub fn seconds(x: f64) -> String {
    let s = format!("{:.3}", x);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".into()
    } else {
        s.into()
    }
}

/// No spreadsheet formulas from names (7.4).
fn csv_safe(v: &str) -> String {
    if v.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{v}")
    } else {
        v.to_string()
    }
}

/// RFC 4180 quoting.
fn csv_field(v: &str) -> String {
    if v.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

/// The cue list: one row per picked cue, silence and fade-out included. `lengths` holds each
/// audio cue's edited length when its audio is there.
pub fn cue_list_csv(
    m: &Manifest,
    picked: &[usize],
    paths: &HashMap<usize, String>,
    lengths: &HashMap<usize, f64>,
) -> Vec<u8> {
    let n = m.cues.len();
    let mut out = String::from("\u{feff}");
    out.push_str(CSV_HEADER);
    out.push_str("\r\n");
    for &i in picked {
        let c = &m.cues[i];
        let audio = c.cue_type == CueType::Audio;
        let group = c
            .group
            .as_deref()
            .and_then(|g| m.groups.iter().find(|x| x.id == g))
            .map(|g| csv_safe(&g.name))
            .unwrap_or_default();
        let if_audio = |v: String| if audio { v } else { String::new() };
        let length = if audio {
            lengths.get(&i).map(|&l| seconds(l)).unwrap_or_default()
        } else {
            seconds(c.duration)
        };
        let row = [
            cue_number(i, n, false),
            csv_safe(&c.name),
            match c.cue_type {
                CueType::Audio => "Audio",
                CueType::Silence => "Silence",
                CueType::Fadeout => "Fade-out",
            }
            .to_string(),
            csv_safe(paths.get(&i).map(String::as_str).unwrap_or("")),
            group,
            if_audio(format!("{}", (c.volume * 100.0).round() as i64)),
            if_audio(seconds(c.fade_in)),
            if_audio(seconds(c.fade_out)),
            seconds(c.pre_wait),
            if_audio(
                if c.looping {
                    "yes (file plays once)"
                } else {
                    "no"
                }
                .to_string(),
            ),
            if c.continue_mode == Continue::AfterEnd {
                "yes"
            } else {
                "no"
            }
            .to_string(),
            length,
        ];
        out.push_str(
            &row.iter()
                .map(|f| csv_field(f))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push_str("\r\n");
    }
    out.into_bytes()
}

// ---------------------------------------------------------------------------
// The zip (7.3)
// ---------------------------------------------------------------------------

/// Writes `<show> audio.zip`: to a temp file beside `out_path`, renamed into place by
/// `finish` (as section 3). Add entries in the spec's order: the cue list, the audio in cue
/// order, the `.tqs` last. Dropped without `finish`, the temp file is removed.
pub struct AudioZip {
    zip: ZipWriter<BufWriter<File>>,
    tmp: tempfile::NamedTempFile,
    out_path: PathBuf,
}

impl AudioZip {
    pub fn create(out_path: &Path) -> Result<Self> {
        let dir = out_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let tmp = tempfile::Builder::new()
            .prefix(".thorncue-export-")
            .suffix(".part")
            .tempfile_in(dir)
            .map_err(TqsError::io("create a temporary file in", dir))?;
        let file = tmp.reopen().map_err(TqsError::io("write", tmp.path()))?;
        Ok(Self {
            zip: ZipWriter::new(BufWriter::new(file)),
            tmp,
            out_path: out_path.to_path_buf(),
        })
    }

    fn ioerr(&self) -> impl Fn(std::io::Error) -> TqsError + '_ {
        move |e| TqsError::Io {
            action: "write",
            path: self.out_path.clone(),
            source: e,
        }
    }

    /// `cue-list.csv`, DEFLATEd.
    pub fn add_cue_list(&mut self, csv: &[u8]) -> Result<()> {
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let path = self.out_path.clone();
        self.zip
            .start_file(CUE_LIST, opts)
            .map_err(|e| zip_io(&path, e))?;
        self.zip.write_all(csv).map_err(self.ioerr())
    }

    /// A file from disk, STORED, copied in chunks. `keep_going` is asked between chunks; when
    /// it says no the export stops with `Ok(false)`.
    pub fn add_file(
        &mut self,
        name: &str,
        src: &Path,
        keep_going: &mut dyn FnMut() -> bool,
    ) -> Result<bool> {
        let size = std::fs::metadata(src)
            .map_err(TqsError::io("read", src))?
            .len();
        let opts = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .large_file(size >= u32::MAX as u64);
        let path = self.out_path.clone();
        self.zip
            .start_file(name, opts)
            .map_err(|e| zip_io(&path, e))?;
        let mut f = BufReader::new(File::open(src).map_err(TqsError::io("read", src))?);
        let mut buf = vec![0u8; 1 << 20];
        loop {
            if !keep_going() {
                return Ok(false);
            }
            let got = f.read(&mut buf).map_err(TqsError::io("read", src))?;
            if got == 0 {
                return Ok(true);
            }
            self.zip.write_all(&buf[..got]).map_err(self.ioerr())?;
        }
    }

    /// Write the comment, fsync and rename into place.
    pub fn finish(mut self) -> Result<()> {
        let path = self.out_path.clone();
        self.zip
            .set_comment(AUDIO_EXPORT_COMMENT)
            .map_err(|e| zip_io(&path, e))?;
        let inner = self.zip.finish().map_err(|e| zip_io(&path, e))?;
        let file = inner.into_inner().map_err(|e| TqsError::Io {
            action: "write",
            path: path.clone(),
            source: e.into_error(),
        })?;
        file.sync_all().map_err(TqsError::io("write", &path))?;
        drop(file);
        self.tmp
            .persist(&path)
            .map_err(|e| TqsError::io("write", &path)(e.error))?;
        crate::read::sync_parent(&path);
        // The temp file was private (0600); an export is meant to be passed on.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
        }
        Ok(())
    }
}

fn zip_io(path: &Path, e: zip::result::ZipError) -> TqsError {
    TqsError::Io {
        action: "write",
        path: path.to_path_buf(),
        source: std::io::Error::other(e),
    }
}

// ---------------------------------------------------------------------------
// Importing one (7.5)
// ---------------------------------------------------------------------------

/// Is this zip an audio export? Its comment says so, or (re-zipped by hand) it has no
/// `manifest.json`, no `show.json` at the top or one folder down, and a top-level
/// `cue-list.csv`.
pub(crate) fn is_audio_export<R: Read + std::io::Seek>(zip: &zip::ZipArchive<R>) -> bool {
    if zip.comment() == AUDIO_EXPORT_COMMENT.as_bytes() {
        return true;
    }
    let mut cue_list = false;
    for name in zip.file_names() {
        let Ok(name) = name else { continue };
        if name == "manifest.json"
            || name == "show.json"
            || (name.ends_with("/show.json") && name.matches('/').count() == 1)
        {
            return false;
        }
        cue_list |= name == CUE_LIST;
    }
    cue_list
}

/// For an audio export: copy the `.tqs` at its top level to `dest` (a file path), after the
/// section 1.3 checks on the zip itself. Errors with the 7.5 refusal when there's none.
pub fn extract_audio_export_tqs(zip_path: &Path, dest: &Path, limits: Limits) -> Result<()> {
    let file = File::open(zip_path).map_err(TqsError::io("open", zip_path))?;
    let mut zip = zipcheck::open(BufReader::new(file))?;
    let (entries, _) = zipcheck::check(&mut zip, &limits, zipcheck::Layout::Legacy)?;
    let found: Vec<_> = entries
        .iter()
        .filter(|e| !e.name.contains('/') && e.name.to_lowercase().ends_with(".tqs"))
        .collect();
    let entry = match found.as_slice() {
        [] => return Err(TqsError::invalid(AUDIO_EXPORT_REFUSED)),
        [e] => *e,
        _ => {
            return Err(TqsError::invalid(
                "This audio export has more than one .tqs in it, so it isn't clear which show to import.",
            ));
        }
    };
    let mut src = zip.by_index(entry.index).map_err(|_| {
        TqsError::invalid(format!("{} is damaged (it can't be unpacked).", entry.name))
    })?;
    let mut out = File::create(dest).map_err(TqsError::io("create", dest))?;
    let mut budget = zipcheck::Budget {
        used: 0,
        max: limits.max_total,
    };
    zipcheck::copy_counted(
        &mut src,
        &mut out,
        entry.size,
        &entry.name,
        &mut budget,
        &mut |_| {},
    )?;
    out.sync_all().map_err(TqsError::io("write", dest))?;
    Ok(())
}
