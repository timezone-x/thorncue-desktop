//! Export audio (FORMAT.md section 7; thorncue-spec#17): the show's cues as `<show> audio.zip`,
//! and one cue as WAV or MP3. Ported from thorncue-web `exports.py` and the "Export audio"
//! dialog in `static/app.js`.
//!
//! Everything here runs on a blocking task, never on the engine thread: it decodes each file
//! itself (kira's decoder, as playback), renders with `engine::cue_samples`, and names, lists
//! and zips with `tqs`. GO and playback don't wait on any of it. One export runs at a time;
//! Stop sets a flag that is checked between cues and between 1 MB chunks of the zip.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use engine::kira::sound::static_sound::StaticSoundData;
use serde::Serialize;
use tqs::{AudioExportOptions, AudioFormat, CueType, Manifest};

/// Progress for the dialog: "Rendering 7 of 24", then "Writing the zip".
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    /// "rendering", "copying" (edits off) or "zipping".
    pub phase: &'static str,
}

/// A finished export.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Done {
    pub files: usize,
    pub file_name: String,
}

/// The dialog's summary line: how many files and roughly how big.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Estimate {
    pub files: usize,
    pub bytes: u64,
}

/// The one export that may run. `start` hands out a stop flag, or `None` if one is running.
#[derive(Default)]
pub struct Jobs {
    running: std::sync::Mutex<Option<Arc<AtomicBool>>>,
}

impl Jobs {
    pub fn start(&self) -> Option<Arc<AtomicBool>> {
        let mut g = self.running.lock().ok()?;
        if g.is_some() {
            return None;
        }
        let flag = Arc::new(AtomicBool::new(false));
        *g = Some(flag.clone());
        Some(flag)
    }

    pub fn finish(&self) {
        if let Ok(mut g) = self.running.lock() {
            *g = None;
        }
    }

    pub fn stop(&self) {
        if let Ok(g) = self.running.lock()
            && let Some(f) = g.as_ref()
        {
            f.store(true, Ordering::Relaxed);
        }
    }
}

/// The cue as the engine renders it.
pub fn cue_render(c: &tqs::Cue) -> engine::CueRender {
    engine::CueRender {
        shape: engine::FadeShape {
            trim_start: c.trim_start,
            trim_end: c.trim_end,
            cuts: c.cuts.clone(),
            fade_in: c.fade_in,
            fade_out: c.fade_out,
            looping: c.looping,
        },
        volume: c.volume,
    }
}

/// The cue's audio file in `show_dir`, if it's an audio cue whose file is there.
fn source(show_dir: &Path, m: &Manifest, i: usize) -> Option<PathBuf> {
    let c = &m.cues[i];
    if c.cue_type != CueType::Audio {
        return None;
    }
    let a = m.assets.get(c.asset.as_deref()?)?;
    let p = show_dir.join(&a.path);
    p.is_file().then_some(p)
}

/// The edited length of cue `c` from the file's header alone (2.5), no decode.
fn edited_seconds(c: &tqs::Cue, duration: f64) -> f64 {
    engine::kept_segments(c.trim_start, c.trim_end, &c.cuts, duration)
        .iter()
        .map(|(s, e)| e - s)
        .sum()
}

/// Files and size the options would give (web `exports.estimate`), from headers only.
pub fn estimate(show_dir: &Path, m: &Manifest, o: &AudioExportOptions) -> Estimate {
    let mut est = Estimate { files: 0, bytes: 0 };
    for i in tqs::picked(m, &o.cues) {
        let Some(p) = source(show_dir, m, i) else {
            continue;
        };
        if !o.edits {
            est.files += 1;
            est.bytes += std::fs::metadata(&p).map(|x| x.len()).unwrap_or(0);
            continue;
        }
        let Ok(info) = tqs::audio_info(&p) else {
            continue;
        };
        let length = edited_seconds(&m.cues[i], info.duration.unwrap_or(0.0));
        if length <= 0.0 {
            continue;
        }
        est.files += 1;
        est.bytes += match o.format {
            AudioFormat::Wav => {
                44 + (length * info.sample_rate as f64).round() as u64 * info.channels as u64 * 2
            }
            AudioFormat::Mp3 => {
                let (_, kbps) = engine::mp3_settings(info.sample_rate);
                (length * kbps as f64 * 1000.0 / 8.0) as u64 + 1024
            }
        };
    }
    est
}

/// Decoded files, one at a time: consecutive cues on the same file share one decode.
struct Decoder {
    last: Option<(PathBuf, StaticSoundData)>,
}

impl Decoder {
    fn get(&mut self, p: &Path) -> Result<&StaticSoundData, String> {
        if self.last.as_ref().map(|(q, _)| q.as_path()) != Some(p) {
            self.last = None;
            let d = StaticSoundData::from_file(p).map_err(|e| e.to_string())?;
            self.last = Some((p.to_path_buf(), d));
        }
        Ok(&self.last.as_ref().unwrap().1)
    }
}

/// Render cue `i` to `dest` as `format`. `Ok(Some(seconds))`, or `Ok(None)` when the edit
/// leaves nothing. Errors are sentences for the operator.
fn render_to(
    dec: &mut Decoder,
    src: &Path,
    m: &Manifest,
    i: usize,
    format: AudioFormat,
    dest: &Path,
) -> Result<Option<f64>, String> {
    let q = tqs::cue_number(i, m.cues.len(), false);
    let raw = dec.get(src).map_err(|e| {
        tracing::error!("decoding {} for export: {e}", src.display());
        format!(
            "{q}'s audio file can't be read. Replace the file, or export with \"Apply edits\" off."
        )
    })?;
    let Some(r) = engine::cue_samples(raw, &cue_render(&m.cues[i])) else {
        return Ok(None);
    };
    let bytes = match format {
        AudioFormat::Wav => engine::encode_wav(&r),
        AudioFormat::Mp3 => engine::encode_mp3(&r).map_err(|e| {
            tracing::error!("encoding {q} as MP3: {e}");
            format!("{q} couldn't be encoded as MP3. Try WAV.")
        })?,
    };
    std::fs::write(dest, bytes).map_err(|e| {
        tracing::error!("writing {}: {e}", dest.display());
        "Couldn't write the export. Check there is free disk space.".to_string()
    })?;
    Ok(Some(r.seconds()))
}

/// Build `<show> audio.zip` at `out` from the show in `show_dir` (manifest `m`, already saved).
/// `work` is an empty scratch folder (removed by the caller). Returns `Ok(None)` if stopped;
/// nothing is left at `out` then.
pub fn build_zip(
    show_dir: &Path,
    m: &Manifest,
    o: &AudioExportOptions,
    out: &Path,
    work: &Path,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<Option<Done>, String> {
    let stopped = || stop.load(Ordering::Relaxed);
    let picked = tqs::picked(m, &o.cues);
    if picked.is_empty() {
        return Err("There are no cues to export.".into());
    }
    let todo: Vec<(usize, PathBuf)> = picked
        .iter()
        .filter_map(|&i| source(show_dir, m, i).map(|p| (i, p)))
        .collect();
    let phase = if o.edits { "rendering" } else { "copying" };
    let total = todo.len();
    progress(Progress {
        done: 0,
        total,
        phase,
    });

    // Each cue's file (rendered into `work`, or the original) and its edited length.
    let mut files: HashMap<usize, (PathBuf, String)> = HashMap::new();
    let mut lengths: HashMap<usize, f64> = HashMap::new();
    let mut dec = Decoder { last: None };
    for (n, (i, src)) in todo.iter().enumerate() {
        if stopped() {
            return Ok(None);
        }
        if o.edits {
            let ext = o.format.ext();
            let dest = work.join(format!("{n}.{ext}"));
            match render_to(&mut dec, src, m, *i, o.format, &dest)? {
                Some(secs) => {
                    files.insert(*i, (dest, ext.to_string()));
                    lengths.insert(*i, secs);
                }
                None => {
                    lengths.insert(*i, 0.0);
                }
            }
        } else {
            let ext = src
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("audio")
                .to_lowercase();
            files.insert(*i, (src.clone(), ext));
            if let Ok(Some(d)) = tqs::audio_info(src).map(|x| x.duration) {
                lengths.insert(*i, edited_seconds(&m.cues[*i], d));
            }
        }
        progress(Progress {
            done: n + 1,
            total,
            phase,
        });
    }

    let (paths, tqs_name) = tqs::plan_paths(m, o, &picked, &|i| {
        files.get(&i).map(|(_, ext)| ext.clone())
    });
    progress(Progress {
        done: total,
        total,
        phase: "zipping",
    });
    let zerr = |e: tqs::TqsError| {
        tracing::error!("writing audio export {}: {e}", out.display());
        e.to_string()
    };
    let mut zip = tqs::AudioZip::create(out).map_err(zerr)?;
    if o.cue_list {
        zip.add_cue_list(&tqs::cue_list_csv(m, &picked, &paths, &lengths))
            .map_err(zerr)?;
    }
    let mut keep_going = || !stopped();
    for i in &picked {
        if let (Some(path), Some((file, _))) = (paths.get(i), files.get(i))
            && !zip.add_file(path, file, &mut keep_going).map_err(zerr)?
        {
            return Ok(None);
        }
    }
    if o.tqs {
        let pkg = work.join("show.tqs");
        tqs::write_package(show_dir, &pkg).map_err(zerr)?;
        if !zip
            .add_file(&tqs_name, &pkg, &mut keep_going)
            .map_err(zerr)?
        {
            return Ok(None);
        }
    }
    if stopped() {
        return Ok(None);
    }
    zip.finish().map_err(zerr)?;
    Ok(Some(Done {
        files: paths.len(),
        file_name: out
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }))
}

/// One cue as WAV or MP3 at `out` (the inspector's Export ▾), written atomically.
pub fn export_cue(
    show_dir: &Path,
    m: &Manifest,
    i: usize,
    format: AudioFormat,
    out: &Path,
) -> Result<(), String> {
    let q = tqs::cue_number(i, m.cues.len(), false);
    let src = source(show_dir, m, i).ok_or_else(|| {
        if m.cues[i].cue_type == CueType::Audio {
            format!("{q} has no audio file. Export it as a cue file.")
        } else {
            "Silence and fade-out cues have no audio. Export them as a cue file.".to_string()
        }
    })?;
    let tmp = tempfile::Builder::new()
        .prefix(".thorncue-export-")
        .suffix(".part")
        .tempfile_in(out.parent().unwrap_or(Path::new(".")))
        .map_err(|e| {
            tracing::error!("temp file beside {}: {e}", out.display());
            "Couldn't write the file. Check the folder exists and there is free disk space."
                .to_string()
        })?;
    let mut dec = Decoder { last: None };
    if render_to(&mut dec, &src, m, i, format, tmp.path())?.is_none() {
        return Err(format!(
            "{q} has no audio left after its trims and cuts, so there's nothing to export."
        ));
    }
    let werr = |e: std::io::Error| {
        tracing::error!("writing {}: {e}", out.display());
        "Couldn't write the file. Check the folder exists and there is free disk space.".to_string()
    };
    tmp.as_file().sync_all().map_err(werr)?;
    tmp.persist(out).map_err(|e| werr(e.error))?;
    // The temp file is owner-only; an export is for sharing (as .tqs).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o644));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn fixture(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../docs/format/fixtures")
            .join(rel)
    }

    /// Fixture 02 (every cue type, seven audio formats, edits) imported into a library.
    fn all_features() -> (tempfile::TempDir, PathBuf, Manifest) {
        let data = tempfile::tempdir().unwrap();
        let lib = crate::library::Library::new(data.path()).unwrap();
        let (id, m, _) = lib
            .import_file(&fixture("valid/02-all-features.tqs"), &mut |_| {})
            .unwrap();
        let dir = lib.show_dir(&id).unwrap();
        (data, dir, m)
    }

    fn run(dir: &Path, m: &Manifest, o: &AudioExportOptions) -> (PathBuf, Done, tempfile::TempDir) {
        let out_dir = tempfile::tempdir().unwrap();
        let out = out_dir
            .path()
            .join(tqs::zip_name(m, &o.cues, &tqs::picked(m, &o.cues)));
        let work = tempfile::tempdir().unwrap();
        let mut seen = Vec::new();
        let done = build_zip(
            dir,
            m,
            o,
            &out,
            work.path(),
            &AtomicBool::new(false),
            &mut |p| seen.push(p),
        )
        .unwrap()
        .unwrap();
        assert_eq!(seen.last().unwrap().phase, "zipping");
        (out, done, out_dir)
    }

    fn entries(zip: &Path) -> Vec<String> {
        // In the zip's own order (file_names() isn't).
        let mut z = zip::ZipArchive::new(std::fs::File::open(zip).unwrap()).unwrap();
        (0..z.len())
            .map(|i| z.by_index_raw(i).unwrap().name().unwrap().to_string())
            .collect()
    }

    fn read(zip: &Path, name: &str) -> Vec<u8> {
        let mut z = zip::ZipArchive::new(std::fs::File::open(zip).unwrap()).unwrap();
        let mut f = z.by_name(name).unwrap();
        let mut v = Vec::new();
        f.read_to_end(&mut v).unwrap();
        v
    }

    #[test]
    fn rendered_export_has_a_file_per_audio_cue_and_the_cue_list() {
        let (_d, dir, m) = all_features();
        let o = AudioExportOptions::default();
        let (out, done, _keep) = run(&dir, &m, &o);
        assert_eq!(done.file_name, "All features audio.zip");
        let names = entries(&out);
        assert_eq!(names[0], "cue-list.csv");
        let audio: Vec<_> = m
            .cues
            .iter()
            .enumerate()
            .filter(|(_, c)| c.cue_type == CueType::Audio && c.asset.is_some())
            .collect();
        assert_eq!(done.files, names.len() - 1);
        assert!(done.files <= audio.len() && done.files > 0);
        for n in &names[1..] {
            assert!(n.ends_with(".wav"), "{n}");
            assert!(n.starts_with('Q'), "numbered: {n}");
            let wav = read(&out, n);
            assert_eq!(&wav[..4], b"RIFF");
        }
        // Each file is exactly that cue's render.
        let first = audio[0].0;
        let path = &names[1];
        let src = source(&dir, &m, first).unwrap();
        let raw = StaticSoundData::from_file(&src).unwrap();
        assert_eq!(
            read(&out, path),
            engine::cue_wav(&raw, &cue_render(&m.cues[first])).unwrap()
        );
        // One cue-list row per cue (silence and fade-out included), plus the header.
        let csv = String::from_utf8(read(&out, "cue-list.csv")).unwrap();
        assert_eq!(csv.trim_end().split("\r\n").count(), m.cues.len() + 1);
        assert!(csv.contains(",Silence,") || csv.contains(",Fade-out,"));
    }

    #[test]
    fn originals_are_byte_for_byte_with_their_own_extension() {
        let (_d, dir, m) = all_features();
        let o = AudioExportOptions {
            edits: false,
            numbers: false,
            ..Default::default()
        };
        let (out, done, _keep) = run(&dir, &m, &o);
        let names = entries(&out);
        assert_eq!(done.files, names.len() - 1);
        let mut exts: Vec<String> = names[1..]
            .iter()
            .map(|n| n.rsplit('.').next().unwrap().to_string())
            .collect();
        exts.sort();
        exts.dedup();
        assert!(exts.len() >= 5, "several formats as they were: {exts:?}");
        // Every file equals its cue's original.
        let picked = tqs::picked(&m, &o.cues);
        let (paths, _) = tqs::plan_paths(&m, &o, &picked, &|i| {
            source(&dir, &m, i).map(|p| p.extension().unwrap().to_string_lossy().to_lowercase())
        });
        for (i, p) in &paths {
            assert_eq!(
                read(&out, p),
                std::fs::read(source(&dir, &m, *i).unwrap()).unwrap(),
                "{p}"
            );
        }
    }

    #[test]
    fn the_tqs_inside_round_trips() {
        let (_d, dir, m) = all_features();
        let o = AudioExportOptions {
            tqs: true,
            format: AudioFormat::Mp3,
            ..Default::default()
        };
        let (out, _, _keep) = run(&dir, &m, &o);
        let names = entries(&out);
        assert_eq!(names.last().unwrap(), "All features.tqs");
        // Importing the zip itself gives the same show (FORMAT.md 7.5).
        let data = tempfile::tempdir().unwrap();
        let lib = crate::library::Library::new(data.path()).unwrap();
        let (_, again, warnings) = lib.import_file(&out, &mut |_| {}).unwrap();
        assert_eq!(warnings[0].0, tqs::AUDIO_EXPORT_IMPORTED);
        assert_eq!(again.cues, m.cues);
        assert!(
            names[1..names.len() - 1]
                .iter()
                .all(|n| n.ends_with(".mp3"))
        );
    }

    #[test]
    fn group_folders_and_a_partial_export() {
        let data = tempfile::tempdir().unwrap();
        let lib = crate::library::Library::new(data.path()).unwrap();
        let (id, m, _) = lib
            .import_file(&fixture("valid/06-groups.tqs"), &mut |_| {})
            .unwrap();
        let dir = lib.show_dir(&id).unwrap();
        let g = m.groups[0].clone();
        let o = AudioExportOptions {
            cues: tqs::CueChoice::Group(g.id.clone()),
            folders: true,
            ..Default::default()
        };
        let (out, done, _keep) = run(&dir, &m, &o);
        assert_eq!(
            done.file_name,
            format!("{} - {} audio.zip", m.show.name, g.name)
        );
        for n in &entries(&out)[1..] {
            assert!(
                n.starts_with(&format!("{}/", tqs::safe_name(&g.name, "Group", 60))),
                "{n}"
            );
        }
    }

    #[test]
    fn stopping_leaves_nothing_behind() {
        let (_d, dir, m) = all_features();
        let out_dir = tempfile::tempdir().unwrap();
        let out = out_dir.path().join("x.zip");
        let work = tempfile::tempdir().unwrap();
        let stop = AtomicBool::new(false);
        let r = build_zip(
            &dir,
            &m,
            &AudioExportOptions::default(),
            &out,
            work.path(),
            &stop,
            &mut |p| {
                if p.done == 1 {
                    stop.store(true, Ordering::Relaxed);
                }
            },
        )
        .unwrap();
        assert!(r.is_none());
        assert_eq!(std::fs::read_dir(out_dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn estimate_counts_the_files() {
        let (_d, dir, m) = all_features();
        let wav = estimate(&dir, &m, &AudioExportOptions::default());
        let mp3 = estimate(
            &dir,
            &m,
            &AudioExportOptions {
                format: AudioFormat::Mp3,
                ..Default::default()
            },
        );
        let orig = estimate(
            &dir,
            &m,
            &AudioExportOptions {
                edits: false,
                ..Default::default()
            },
        );
        assert!(wav.files > 0 && wav.files == mp3.files && orig.files >= wav.files);
        assert!(wav.bytes > 0 && mp3.bytes > 0 && orig.bytes > 0);
        // Matches what the export writes.
        let (out, done, _keep) = run(&dir, &m, &AudioExportOptions::default());
        assert_eq!(done.files, wav.files);
        let real: u64 = {
            let mut z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
            (1..z.len())
                .map(|i| z.by_index_raw(i).unwrap().size())
                .sum()
        };
        let ratio = real as f64 / wav.bytes as f64;
        assert!(
            (0.4..=1.1).contains(&ratio),
            "estimate {} vs {real}",
            wav.bytes
        );
    }

    #[test]
    fn one_cue_as_wav_and_mp3() {
        let (_d, dir, m) = all_features();
        let i = m.cues.iter().position(|c| c.asset.is_some()).unwrap();
        let out_dir = tempfile::tempdir().unwrap();
        for f in [AudioFormat::Wav, AudioFormat::Mp3] {
            let out = out_dir.path().join(tqs::single_file_name(&m, i, f.ext()));
            export_cue(&dir, &m, i, f, &out).unwrap();
            assert!(std::fs::metadata(&out).unwrap().len() > 1000);
        }
        let silence = m
            .cues
            .iter()
            .position(|c| c.cue_type != CueType::Audio)
            .unwrap();
        let e = export_cue(
            &dir,
            &m,
            silence,
            AudioFormat::Wav,
            &out_dir.path().join("s.wav"),
        )
        .unwrap_err();
        assert!(e.contains("no audio"), "{e}");
        assert_eq!(std::fs::read_dir(out_dir.path()).unwrap().count(), 2);
    }
}
