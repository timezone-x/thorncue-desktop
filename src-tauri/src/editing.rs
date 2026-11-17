//! Show edits (M6), ported from reference/web/app.js (inspector, add/duplicate/delete) and
//! the web's cue routes. Pure functions on a `Manifest`: every edit goes through
//! `tqs::normalise_manifest`, so an edited show is always a valid `.tqs` by the same rules as
//! an import (FORMAT.md 2.4: types checked, numbers clamped, cuts normalised, unknown fields
//! kept, unused audio dropped).

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tqs::Manifest;

/// Cue fields the inspector may change (M7 adds the trim fields).
const EDITABLE: [&str; 11] = [
    "name",
    "volume",
    "fade_in",
    "fade_out",
    "pre_wait",
    "duration",
    "loop",
    "continue",
    "trim_start",
    "trim_end",
    "cuts",
];

/// An audio file copied into the show folder, ready to become an asset.
#[derive(Debug, Clone)]
pub struct NewAudio {
    pub asset_id: String,
    /// `audio/<asset_id>.<ext>`
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    /// The file's name without extension, for the cue name.
    pub name: String,
}

#[derive(Debug, Clone)]
pub enum Edit {
    /// Change some fields of a cue.
    Update {
        id: String,
        patch: Map<String, Value>,
    },
    /// Move a cue to position `to` (0-based, in the list after removal).
    Move {
        id: String,
        to: usize,
    },
    /// New audio cues at the end (as the web's Add cue).
    AddAudio(Vec<NewAudio>),
    /// "Silence" or "Fade out all", 2 s (as the web).
    AddControl {
        silence: bool,
    },
    /// A copy right after the original, sharing its audio, named "<name> copy" (as the web).
    Duplicate {
        id: String,
    },
    /// Point an audio cue at a new file; trims and cuts are cleared (they belonged to the old file).
    ReplaceAudio {
        id: String,
        audio: NewAudio,
    },
    Delete {
        id: String,
    },
    ShowName(String),
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn cue_index(cues: &[Value], id: &str) -> Result<usize, String> {
    cues.iter()
        .position(|c| c.get("id").and_then(Value::as_str) == Some(id))
        .ok_or_else(|| "That cue is no longer in the show.".to_string())
}

fn audio_cue(id: &str, name: &str, asset: &str) -> Value {
    json!({
        "id": id, "type": "audio", "name": name, "asset": asset, "volume": 1.0, "fade_in": 0.0,
        "fade_out": 0.0, "pre_wait": 0.0, "duration": 0.0, "loop": false, "continue": "none",
        "trim_start": 0.0, "trim_end": null, "cuts": []
    })
}

fn group_of(cue: Option<&Value>) -> Option<&str> {
    cue.and_then(|c| c.get("group")).and_then(Value::as_str)
}

/// Which group a cue belongs to once it is dropped at `to` in `cues` (the list without it).
///
/// A group's cues must stay consecutive (FORMAT.md 2.7); if a move left them apart, the next
/// read would pull them back together and the cue would jump away from where it was put. So
/// membership follows where the cue lands:
/// - between two cues of one group: it joins that group;
/// - next to a cue of its own group (inside or at either end): it stays;
/// - it was its group's only cue: it keeps the group, so the group isn't lost;
/// - anywhere else: it leaves its group.
fn group_after_move(cues: &[Value], cue: &Value, to: usize) -> Option<String> {
    let prev = to.checked_sub(1).and_then(|j| group_of(cues.get(j)));
    let next = group_of(cues.get(to));
    if prev.is_some() && prev == next {
        return prev.map(String::from);
    }
    let own = group_of(Some(cue))?;
    let alone = !cues.iter().any(|c| group_of(Some(c)) == Some(own));
    (alone || prev == Some(own) || next == Some(own)).then(|| own.to_string())
}

/// Apply `edit`. Returns the new manifest and the id of the cue to inspect next, if any.
pub fn apply(manifest: &Manifest, edit: Edit) -> Result<(Manifest, Option<String>), String> {
    let mut v = serde_json::to_value(manifest).map_err(|e| e.to_string())?;
    let mut focus = None;
    {
        let obj = v.as_object_mut().expect("a manifest is an object");
        match edit {
            Edit::Update { id, patch } => {
                let cues = obj["cues"].as_array_mut().expect("cues");
                let i = cue_index(cues, &id)?;
                let cue = cues[i].as_object_mut().expect("cue");
                for (k, val) in patch {
                    if !EDITABLE.contains(&k.as_str()) {
                        return Err(format!("{k} can't be changed here."));
                    }
                    cue.insert(k, val);
                }
                focus = Some(id);
            }
            Edit::Move { id, to } => {
                let cues = obj["cues"].as_array_mut().expect("cues");
                let i = cue_index(cues, &id)?;
                let mut c = cues.remove(i);
                let to = to.min(cues.len());
                match group_after_move(cues, &c, to) {
                    Some(g) => c["group"] = json!(g),
                    None => {
                        c.as_object_mut().expect("cue").remove("group");
                    }
                }
                cues.insert(to, c);
                focus = Some(id);
            }
            Edit::AddAudio(files) => {
                for f in files {
                    obj["assets"].as_object_mut().expect("assets").insert(
                        f.asset_id.clone(),
                        json!({"path": f.path, "sha256": f.sha256, "bytes": f.bytes}),
                    );
                    let id = new_id();
                    obj["cues"].as_array_mut().expect("cues").push(audio_cue(
                        &id,
                        &f.name,
                        &f.asset_id,
                    ));
                    focus = Some(id);
                }
            }
            Edit::AddControl { silence } => {
                let id = new_id();
                obj["cues"].as_array_mut().expect("cues").push(json!({
                    "id": id, "type": if silence { "silence" } else { "fadeout" },
                    "name": if silence { "Silence" } else { "Fade out all" },
                    "asset": null, "volume": 1.0, "fade_in": 0.0, "fade_out": 0.0, "pre_wait": 0.0,
                    "duration": 2.0, "loop": false, "continue": "none", "trim_start": 0.0,
                    "trim_end": null, "cuts": []
                }));
                focus = Some(id);
            }
            Edit::Duplicate { id } => {
                let cues = obj["cues"].as_array_mut().expect("cues");
                let i = cue_index(cues, &id)?;
                let mut copy = cues[i].clone();
                let new = new_id();
                let name = copy
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let name: String = format!("{name} copy").chars().take(100).collect();
                copy["id"] = json!(new);
                copy["name"] = json!(name);
                cues.insert(i + 1, copy);
                focus = Some(new);
            }
            Edit::ReplaceAudio { id, audio } => {
                obj["assets"].as_object_mut().expect("assets").insert(
                    audio.asset_id.clone(),
                    json!({"path": audio.path, "sha256": audio.sha256, "bytes": audio.bytes}),
                );
                let cues = obj["cues"].as_array_mut().expect("cues");
                let i = cue_index(cues, &id)?;
                if cues[i]["type"] != "audio" {
                    return Err("Only audio cues have a file to replace.".into());
                }
                cues[i]["asset"] = json!(audio.asset_id);
                cues[i]["trim_start"] = json!(0.0);
                cues[i]["trim_end"] = Value::Null;
                cues[i]["cuts"] = json!([]);
                focus = Some(id);
            }
            Edit::Delete { id } => {
                let cues = obj["cues"].as_array_mut().expect("cues");
                let i = cue_index(cues, &id)?;
                cues.remove(i);
                focus = cues
                    .get(i.min(cues.len().saturating_sub(1)))
                    .and_then(|c| c["id"].as_str())
                    .map(String::from);
            }
            Edit::ShowName(name) => {
                obj["show"]["name"] = json!(name);
            }
        }
    }
    let (m, _) =
        tqs::normalise_manifest(&v).map_err(|e| format!("That change isn't valid: {e}"))?;
    Ok((m, focus))
}

/// Copy an audio file into `show_dir/audio/` under a new asset id, checking it plays first.
/// Written to a temp file and renamed, so a failed copy leaves nothing.
/// Natural file-name order, as the web's `localeCompare(b, undefined, { numeric: true,
/// sensitivity: "base" })` (thorncue-spec#4): digit runs compare as numbers ("2" before "10"),
/// letters ignore case. Ties fall back to the plain string so the order is always the same.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    fn runs(s: &str) -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = Vec::new();
        for ch in s.chars() {
            let digit = ch.is_ascii_digit();
            match out.last_mut() {
                Some((d, run)) if *d == digit => run.push(ch),
                _ => out.push((digit, ch.to_string())),
            }
        }
        out
    }
    let (ra, rb) = (runs(a), runs(b));
    for ((da, x), (db, y)) in ra.iter().zip(rb.iter()) {
        let ord = match (da, db) {
            (true, true) => {
                let (tx, ty) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty))
            }
            _ => x.to_lowercase().cmp(&y.to_lowercase()),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    ra.len().cmp(&rb.len()).then_with(|| a.cmp(b))
}

/// Sort paths for adding as cues: natural order of their file names (thorncue-spec#4).
pub fn sort_by_file_name(paths: &mut [std::path::PathBuf]) {
    let name = |p: &std::path::PathBuf| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    paths.sort_by(|a, b| natural_cmp(&name(a), &name(b)));
}

/// Copy many files in natural name order; a file that fails is skipped and its reason kept,
/// so the rest still go in (thorncue-spec#4, as the web's `addAudioFiles`).
pub fn copy_many(show_dir: &Path, paths: Vec<std::path::PathBuf>) -> (Vec<NewAudio>, Vec<String>) {
    let mut paths = paths;
    sort_by_file_name(&mut paths);
    let mut files = Vec::new();
    let mut problems = Vec::new();
    for p in paths {
        match copy_audio_in(show_dir, &p) {
            Ok(f) => files.push(f),
            Err(e) => problems.push(e),
        }
    }
    (files, problems)
}

pub fn copy_audio_in(show_dir: &Path, src: &Path) -> Result<NewAudio, String> {
    let file_name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = src
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .filter(|e| tqs::AUDIO_EXTS.contains(&e.as_str()))
        .ok_or_else(|| {
            format!(
                "{file_name} isn't a file ThornCue can play (use MP3, WAV, OGG, FLAC, M4A or AIFF)."
            )
        })?;
    if tqs::check_audio_file(src).is_err() {
        return Err(format!(
            "{file_name} isn't audio ThornCue can play. It may be damaged."
        ));
    }
    let asset_id = new_id();
    let rel = format!("audio/{asset_id}.{ext}");
    let audio_dir = show_dir.join("audio");
    std::fs::create_dir_all(&audio_dir)
        .map_err(|e| format!("Couldn't prepare the show folder: {e}."))?;
    let disk = |e: std::io::Error| {
        format!("Couldn't copy {file_name} into the show: {e}. Check there is free disk space.")
    };
    let mut input = File::open(src).map_err(|e| format!("Couldn't open {file_name}: {e}."))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".add-")
        .tempfile_in(&audio_dir)
        .map_err(disk)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut bytes = 0u64;
    loop {
        let n = input.read(&mut buf).map_err(disk)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        tmp.write_all(&buf[..n]).map_err(disk)?;
        bytes += n as u64;
    }
    if bytes == 0 {
        return Err(format!("{file_name} is empty."));
    }
    tmp.as_file().sync_all().map_err(disk)?;
    tmp.persist(show_dir.join(&rel))
        .map_err(|e| disk(e.error))?;
    let sha256 = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().trim().to_string())
        .unwrap_or_default();
    let name = if stem.is_empty() {
        "Untitled cue".into()
    } else {
        stem.chars().take(100).collect()
    };
    Ok(NewAudio {
        asset_id,
        path: rel,
        sha256,
        bytes,
        name,
    })
}

/// A new, empty show manifest.
pub fn empty_show(name: &str) -> Manifest {
    let v = json!({
        "format": tqs::FORMAT, "format_version": tqs::FORMAT_VERSION,
        "generator": {"name": "thorncue-desktop", "version": env!("CARGO_PKG_VERSION")},
        "exported_at": "", "show": {"name": name, "author": "", "notes": "", "source": null},
        "assets": {}, "cues": []
    });
    tqs::normalise_manifest(&v)
        .expect("an empty show is valid")
        .0
}

#[cfg(test)]
mod natural_order_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn files_are_added_in_natural_name_order() {
        let mut paths: Vec<PathBuf> = [
            "/a/Q10 door.wav",
            "/b/q2 Bell.mp3",
            "/a/Q1 preshow.wav",
            "/c/Q02 thunder.wav",
            "/a/intro.wav",
            "/a/Q2 bell.wav",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        sort_by_file_name(&mut paths);
        let names: Vec<_> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "intro.wav",
                "Q1 preshow.wav",
                "q2 Bell.mp3",
                "Q2 bell.wav",
                "Q02 thunder.wav",
                "Q10 door.wav"
            ]
        );
    }

    #[test]
    fn long_numbers_dont_overflow() {
        use std::cmp::Ordering::Less;
        assert_eq!(
            natural_cmp(
                "track 99999999999999999999999",
                "track 100000000000000000000000"
            ),
            Less
        );
        assert_eq!(natural_cmp("a", "a1"), Less);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(rel: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../docs/format/fixtures")
            .join(rel)
    }

    fn patch(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn updates_are_validated_and_clamped_like_an_import() {
        let (m, _) = apply(&empty_show("S"), Edit::AddControl { silence: true }).unwrap();
        let id = m.cues[0].id.clone();
        let (m, _) = apply(
            &m,
            Edit::Update {
                id: id.clone(),
                patch: patch(json!({"duration": 9000, "name": "  "})),
            },
        )
        .unwrap();
        assert_eq!(m.cues[0].duration, 3600.0);
        assert_eq!(m.cues[0].name, "Untitled cue");
        assert!(
            apply(
                &m,
                Edit::Update {
                    id: id.clone(),
                    patch: patch(json!({"volume": "loud"}))
                }
            )
            .is_err()
        );
        assert!(
            apply(
                &m,
                Edit::Update {
                    id,
                    patch: patch(json!({"type": "audio"}))
                }
            )
            .is_err(),
            "type can't change"
        );
    }

    /// Fixture 06's cues and groups: g01 | act1: g02 g03 g04 | g05 | storm: g06 g07.
    fn grouped_show() -> Manifest {
        let dir = tempfile::tempdir().unwrap();
        tqs::read_package(
            &fixture("valid/06-groups.tqs"),
            &dir.path().join("s"),
            tqs::Limits::default(),
        )
        .unwrap()
        .0
    }

    fn layout(m: &Manifest) -> Vec<String> {
        m.cues
            .iter()
            .map(|c| match &c.group {
                Some(g) => format!("{}:{g}", c.id),
                None => c.id.clone(),
            })
            .collect()
    }

    fn mv(m: &Manifest, id: &str, to: usize) -> Manifest {
        apply(m, Edit::Move { id: id.into(), to }).unwrap().0
    }

    #[test]
    fn many_files_go_in_name_order_and_a_bad_one_is_skipped() {
        let src = tempfile::tempdir().unwrap();
        let show = tempfile::tempdir().unwrap();
        for (from, to) in [
            ("audio/tone-e330.mp3", "Q10 door.mp3"),
            ("audio/tone-a440.wav", "Q2 bell.wav"),
            ("audio/not-audio.txt", "Q3 broken.wav"),
            ("audio/tone-g392.flac", "Q1 preshow.flac"),
        ] {
            std::fs::copy(fixture(from), src.path().join(to)).unwrap();
        }
        let paths = [
            "Q10 door.mp3",
            "Q3 broken.wav",
            "Q2 bell.wav",
            "Q1 preshow.flac",
        ]
        .iter()
        .map(|n| src.path().join(n))
        .collect();
        let (files, problems) = copy_many(show.path(), paths);
        let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Q1 preshow", "Q2 bell", "Q10 door"]);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("Q3 broken.wav"), "{problems:?}");
        let (m, _) = apply(&empty_show("S"), Edit::AddAudio(files)).unwrap();
        let cue_names: Vec<_> = m.cues.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(cue_names, ["Q1 preshow", "Q2 bell", "Q10 door"]);
    }

    #[test]
    fn fixture_06_layout() {
        let m = grouped_show();
        assert_eq!(
            layout(&m),
            [
                "g01",
                "g02:act1",
                "g03:act1",
                "g04:act1",
                "g05",
                "g06:storm",
                "g07:storm"
            ]
        );
    }

    /// Every edit keeps the groups (a group-free editor used to be able to split them, and a
    /// 1.1 reader would then reorder the show to put them back together).
    #[test]
    fn edits_keep_groups() {
        let m = grouped_show();
        let (u, _) = apply(
            &m,
            Edit::Update {
                id: "g03".into(),
                patch: patch(json!({"volume": 0.5})),
            },
        )
        .unwrap();
        assert_eq!(u.groups, m.groups);
        assert_eq!(layout(&u), layout(&m));

        let (d, new) = apply(&m, Edit::Duplicate { id: "g04".into() }).unwrap();
        assert_eq!(
            d.cues[4].group.as_deref(),
            Some("act1"),
            "a copy stays in the group"
        );
        assert_eq!(d.cues[4].id, new.unwrap());

        let (a, _) = apply(&m, Edit::AddControl { silence: true }).unwrap();
        assert_eq!(
            a.cues.last().unwrap().group,
            None,
            "new cues go at the end, ungrouped"
        );
        assert_eq!(a.groups, m.groups);

        let (n, _) = apply(&m, Edit::ShowName("Renamed".into())).unwrap();
        assert_eq!(n.groups, m.groups);
        assert_eq!(layout(&n), layout(&m));
    }

    #[test]
    fn moving_a_cue_out_of_its_group_leaves_it() {
        // g03 to the very top: the show order is exactly what was asked for.
        let m = mv(&grouped_show(), "g03", 0);
        assert_eq!(
            layout(&m),
            [
                "g03",
                "g01",
                "g02:act1",
                "g04:act1",
                "g05",
                "g06:storm",
                "g07:storm"
            ]
        );
        // g02 between g05 and g06, next to neither of its own: leaves.
        let m = mv(&grouped_show(), "g02", 4);
        assert_eq!(
            layout(&m),
            [
                "g01",
                "g03:act1",
                "g04:act1",
                "g05",
                "g02",
                "g06:storm",
                "g07:storm"
            ]
        );
    }

    #[test]
    fn moving_a_cue_into_a_group_joins_it() {
        // g05 between g06 and g07.
        let m = mv(&grouped_show(), "g05", 5);
        assert_eq!(
            layout(&m),
            [
                "g01",
                "g02:act1",
                "g03:act1",
                "g04:act1",
                "g06:storm",
                "g05:storm",
                "g07:storm"
            ]
        );
        // g02 between g06 and g07: joins Storm, Act 1 keeps its other cues.
        let m = mv(&grouped_show(), "g02", 5);
        assert_eq!(
            layout(&m),
            [
                "g01",
                "g03:act1",
                "g04:act1",
                "g05",
                "g06:storm",
                "g02:storm",
                "g07:storm"
            ]
        );
    }

    #[test]
    fn moving_within_or_to_the_edge_of_its_group_keeps_it() {
        // `to` counts in the list without the moved cue: 2 is between g03 and g04.
        let m = mv(&grouped_show(), "g02", 2);
        assert_eq!(
            layout(&m),
            [
                "g01",
                "g03:act1",
                "g02:act1",
                "g04:act1",
                "g05",
                "g06:storm",
                "g07:storm"
            ]
        );
        // Right after its group's last cue: still in it.
        let m = mv(&grouped_show(), "g02", 3);
        assert_eq!(
            layout(&m),
            [
                "g01",
                "g03:act1",
                "g04:act1",
                "g02:act1",
                "g05",
                "g06:storm",
                "g07:storm"
            ]
        );
        // g06 to after g07, the end of the show.
        let m = mv(&grouped_show(), "g06", 6);
        assert_eq!(
            layout(&m),
            [
                "g01",
                "g02:act1",
                "g03:act1",
                "g04:act1",
                "g05",
                "g07:storm",
                "g06:storm"
            ]
        );
    }

    #[test]
    fn a_groups_only_cue_takes_the_group_with_it() {
        let mut m = grouped_show();
        for id in ["g03", "g04"] {
            m = apply(&m, Edit::Delete { id: id.into() }).unwrap().0;
        }
        let m = mv(&m, "g02", 4);
        assert_eq!(
            layout(&m),
            ["g01", "g05", "g06:storm", "g07:storm", "g02:act1"]
        );
        assert_eq!(m.groups.len(), 2);
    }

    #[test]
    fn deleting_a_groups_last_cue_drops_the_group() {
        let mut m = grouped_show();
        for id in ["g06", "g07"] {
            m = apply(&m, Edit::Delete { id: id.into() }).unwrap().0;
        }
        assert_eq!(m.groups.len(), 1);
        assert_eq!(m.groups[0].id, "act1");
    }

    #[test]
    fn group_membership_isnt_an_inspector_field() {
        let r = apply(
            &grouped_show(),
            Edit::Update {
                id: "g01".into(),
                patch: patch(json!({"group": "storm"})),
            },
        );
        assert!(r.is_err());
    }

    #[test]
    fn add_duplicate_move_delete() {
        let dir = tempfile::tempdir().unwrap();
        let a = copy_audio_in(dir.path(), &fixture("audio/tone-a440.wav")).unwrap();
        assert!(dir.path().join(&a.path).is_file());
        let (m, focus) = apply(&empty_show("S"), Edit::AddAudio(vec![a.clone()])).unwrap();
        assert_eq!(m.cues[0].name, "tone-a440");
        assert_eq!(m.assets[&a.asset_id].bytes, a.bytes);
        let id = focus.unwrap();
        let (m, copy) = apply(&m, Edit::Duplicate { id: id.clone() }).unwrap();
        assert_eq!(m.cues.len(), 2);
        assert_eq!(m.cues[1].name, "tone-a440 copy");
        assert_eq!(
            m.cues[1].asset, m.cues[0].asset,
            "a duplicate shares the asset"
        );
        assert_eq!(m.assets.len(), 1);
        let (m, _) = apply(&m, Edit::AddControl { silence: false }).unwrap();
        assert_eq!(m.cues[2].name, "Fade out all");
        assert_eq!(m.cues[2].duration, 2.0);
        let (m, _) = apply(
            &m,
            Edit::Move {
                id: m.cues[2].id.clone(),
                to: 0,
            },
        )
        .unwrap();
        assert_eq!(m.cues[0].name, "Fade out all");
        let (m, _) = apply(&m, Edit::Delete { id: id.clone() }).unwrap();
        let (m, _) = apply(&m, Edit::Delete { id: copy.unwrap() }).unwrap();
        assert!(
            m.assets.is_empty(),
            "audio no cue uses is dropped from the manifest"
        );
    }

    #[test]
    fn replacing_a_file_clears_its_edits() {
        let dir = tempfile::tempdir().unwrap();
        let a = copy_audio_in(dir.path(), &fixture("audio/tone-a440.wav")).unwrap();
        let (m, focus) = apply(&empty_show("S"), Edit::AddAudio(vec![a])).unwrap();
        let id = focus.unwrap();
        let (m, _) = apply(
            &m,
            Edit::Update {
                id: id.clone(),
                patch: patch(json!({"trim_start": 0.5, "cuts": [[1, 1.2]]})),
            },
        )
        .unwrap();
        let b = copy_audio_in(dir.path(), &fixture("audio/tone-g392.flac")).unwrap();
        let (m, _) = apply(
            &m,
            Edit::ReplaceAudio {
                id,
                audio: b.clone(),
            },
        )
        .unwrap();
        assert_eq!(m.cues[0].asset.as_deref(), Some(b.asset_id.as_str()));
        assert_eq!(m.cues[0].trim_start, 0.0);
        assert!(m.cues[0].cuts.is_empty());
        assert_eq!(m.assets.len(), 1);
    }

    #[test]
    fn files_that_are_not_audio_are_refused_and_leave_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake.wav");
        std::fs::write(&fake, "not audio").unwrap();
        let show = dir.path().join("show");
        assert!(copy_audio_in(&show, &fake).is_err());
        assert!(copy_audio_in(&show, &fixture("audio/not-audio.txt")).is_err());
        assert!(
            !show.join("audio").exists()
                || std::fs::read_dir(show.join("audio")).unwrap().count() == 0
        );
    }
}
