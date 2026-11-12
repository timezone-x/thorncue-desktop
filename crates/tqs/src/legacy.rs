//! Legacy ThornCue Web "Export ZIP" (FORMAT.md section 5): `<CODE>/show.json` and
//! `<CODE>/audio/<cue-id>.<ext>`.
//!
//! Differences from `tqs_ref.py convert_legacy`, where the spec says otherwise:
//! - every rule in 1.3 applies (case collisions, compression method, ratio, running byte
//!   count), not only names, encryption, links and the declared total;
//! - the result goes straight into a library folder (unpacked `.tqs`) instead of a new zip;
//! - audio files no cue uses are reported as warnings instead of silently skipped.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::audio::check_audio_file;
use crate::error::{Result, TqsError, Warning};
use crate::manifest::{AUDIO_EXTS, FORMAT, FORMAT_VERSION, Manifest, normalise_manifest};
use crate::read::{make_temp_dir, move_into_place, write_synced};
use crate::time::now_rfc3339;
use crate::zipcheck::{self, Budget, Layout};
use crate::{Limits, MAX_MANIFEST_BYTES};

/// Python's truthiness, for the loosely-typed legacy booleans.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Lowercase and keep only `[a-z0-9_-]`, at most 64 characters.
fn clean_id(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
        .take(64)
        .collect()
}

/// Convert a legacy web export into a library folder at `dest_dir` (which must not exist).
pub fn import_legacy_zip(
    path: &Path,
    dest_dir: &Path,
    limits: Limits,
) -> Result<(Manifest, Vec<Warning>)> {
    let file = File::open(path).map_err(TqsError::io("open", path))?;
    let mut zip = zipcheck::open(BufReader::new(file)).map_err(|_| {
        TqsError::invalid("This file isn't a ThornCue show or web export (it isn't a zip archive, or it is damaged).")
    })?;
    let (entries, mut warnings) = zipcheck::check(&mut zip, &limits, Layout::Legacy)?;
    let by_name: HashMap<&str, &zipcheck::Entry> =
        entries.iter().map(|e| (e.name.as_str(), e)).collect();

    let show_entry = entries
        .iter()
        .find(|e| {
            e.name == "show.json"
                || (e.name.ends_with("/show.json") && e.name.matches('/').count() == 1)
        })
        .ok_or_else(|| {
            TqsError::invalid("This zip has no show.json, so it isn't a ThornCue export.")
        })?;
    if show_entry.size > MAX_MANIFEST_BYTES {
        return Err(TqsError::invalid("show.json is too large."));
    }
    let mut data = Vec::new();
    zip.by_index(show_entry.index)
        .map_err(|_| TqsError::invalid("show.json is damaged."))?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|_| TqsError::invalid("show.json is damaged."))?;
    let legacy: Value = std::str::from_utf8(&data)
        .ok()
        .and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok())
        .ok_or_else(|| TqsError::invalid("show.json isn't valid JSON."))?;
    let legacy_cues = legacy
        .as_object()
        .and_then(|o| o.get("cues"))
        .and_then(Value::as_array)
        .ok_or_else(|| TqsError::invalid("show.json has no cue list."))?;
    let prefix = &show_entry.name[..show_entry.name.len() - "show.json".len()];

    let tmp = make_temp_dir(dest_dir)?;
    let audio_dir = tmp.path().join("audio");
    std::fs::create_dir(&audio_dir).map_err(TqsError::io("create", &audio_dir))?;

    let mut budget = Budget {
        used: 0,
        max: limits.max_total,
    };
    let mut assets: Map<String, Value> = Map::new();
    let mut used_members: HashSet<String> = HashSet::new();
    let mut cues = Vec::new();
    for (i, c) in legacy_cues.iter().enumerate() {
        let Some(c) = c.as_object() else { continue };
        let cue_type = c
            .get("type")
            .filter(|v| truthy(Some(v)))
            .cloned()
            .unwrap_or_else(|| json!("audio"));
        let raw_id = match c.get("id") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        let mut id = clean_id(&raw_id);
        if id.is_empty() {
            id = format!("cue{i}");
        }

        let mut asset = Value::Null;
        let filename = c.get("filename").and_then(Value::as_str).unwrap_or("");
        if cue_type == "audio" && !filename.is_empty() {
            let (stem, ext) = filename.rsplit_once('.').unwrap_or(("", filename));
            let stem = clean_id(stem);
            let ext = ext.to_lowercase();
            let member = format!("{prefix}audio/{filename}");
            match by_name.get(member.as_str()) {
                Some(entry) if !stem.is_empty() && AUDIO_EXTS.contains(&ext.as_str()) => {
                    if !assets.contains_key(&stem) {
                        let rel = format!("audio/{stem}.{ext}");
                        let out_path = tmp.path().join(&rel);
                        let mut out =
                            File::create(&out_path).map_err(TqsError::io("create", &out_path))?;
                        let mut src = zip.by_index(entry.index).map_err(|_| {
                            TqsError::invalid(format!(
                                "{filename} is damaged (it can't be unpacked)."
                            ))
                        })?;
                        let (n, sha) = zipcheck::copy_counted(
                            &mut src,
                            &mut out,
                            entry.size,
                            &member,
                            &mut budget,
                            &mut |_| {},
                        )?;
                        drop(src);
                        if n != entry.size {
                            return Err(TqsError::invalid(format!(
                                "{filename} is damaged (wrong size)."
                            )));
                        }
                        if n == 0 {
                            return Err(TqsError::invalid(format!("{filename} is empty.")));
                        }
                        out.sync_all().map_err(TqsError::io("write", &out_path))?;
                        drop(out);
                        if check_audio_file(&out_path).is_err() {
                            return Err(TqsError::invalid(format!(
                                "{filename} isn't audio ThornCue can play. {}",
                                crate::audio::UNPLAYABLE_HINT
                            )));
                        }
                        assets.insert(
                            stem.clone(),
                            json!({ "path": rel, "sha256": sha, "bytes": n }),
                        );
                    }
                    used_members.insert(member);
                    asset = json!(stem);
                }
                _ => {
                    let name = c.get("name").and_then(Value::as_str).unwrap_or("?");
                    warnings.push(Warning(format!(
                        "Cue \"{name}\": audio file {filename} is missing; importing it as missing."
                    )));
                }
            }
        }
        cues.push(json!({
            "id": id,
            "type": cue_type,
            "name": c.get("name").cloned().unwrap_or(Value::Null),
            "asset": asset,
            "volume": c.get("volume").cloned().unwrap_or(Value::Null),
            "fade_in": c.get("fade_in").cloned().unwrap_or(Value::Null),
            "fade_out": c.get("fade_out").cloned().unwrap_or(Value::Null),
            "pre_wait": c.get("pre_wait").cloned().unwrap_or(Value::Null),
            "duration": c.get("duration").cloned().unwrap_or(Value::Null),
            "loop": truthy(c.get("loop")),
            "continue": if truthy(c.get("auto_continue")) { "after_end" } else { "none" },
            "trim_start": c.get("trim_start").cloned().unwrap_or(Value::Null),
            "trim_end": c.get("trim_end").cloned().unwrap_or(Value::Null),
            "cuts": c.get("cuts").cloned().unwrap_or(Value::Null),
        }));
    }
    let audio_prefix = format!("{prefix}audio/");
    for e in &entries {
        if e.name.starts_with(&audio_prefix) && !used_members.contains(&e.name) {
            warnings.push(Warning(format!("Skipping {}: no cue uses it.", e.name)));
        }
    }

    let lo = legacy.as_object().expect("checked above");
    let raw = json!({
        "format": FORMAT,
        "format_version": FORMAT_VERSION,
        "generator": { "name": "thorncue-desktop legacy import", "version": env!("CARGO_PKG_VERSION") },
        "exported_at": now_rfc3339(),
        "show": {
            "name": lo.get("name").cloned().unwrap_or(Value::Null),
            "author": lo.get("author").cloned().unwrap_or(json!("")),
            "notes": lo.get("notes").cloned().unwrap_or(json!("")),
            "source": Value::Null,
        },
        "assets": assets,
        "cues": cues,
    });
    let (manifest, w2) = normalise_manifest(&raw)?;
    warnings.extend(w2);

    write_synced(
        &tmp.path().join("manifest.json"),
        &manifest.to_json_pretty(),
    )?;
    move_into_place(tmp, dest_dir)?;
    Ok((manifest, warnings))
}
