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
    /// Put cues in a new group (thorncue-spec#2, web `openGroupDialog` / group form): they are
    /// pulled together at the first one's place. Refused if they sit in the middle of another
    /// group, which would split it.
    CreateGroup {
        ids: Vec<String>,
        name: String,
        color: String,
    },
    /// Rename or recolour a group.
    UpdateGroup {
        id: String,
        name: String,
        color: String,
    },
    /// Remove a group; its cues stay where they are, ungrouped.
    Ungroup {
        id: String,
    },
    /// Move cues (kept in their order) to just before or after `anchor` (web `moveCues`).
    /// `group`: `None` leaves membership alone (dragging a whole group); `Some(g)` sets it on
    /// every moved cue (dragging a cue: it joins the group of the row it lands next to).
    MoveCues {
        ids: Vec<String>,
        anchor: String,
        after: bool,
        group: Option<Option<String>>,
    },
    /// The inspector's Group field (web `f-group`): joining a group puts the cue at its end;
    /// leaving keeps the cue in place if it was the group's first, else it goes just below.
    SetGroup {
        id: String,
        group: Option<String>,
    },
    /// A `.tqc`/`.tqg`'s cues added at the end (FORMAT.md 6.4), already given new ids by
    /// [`prepare_piece`].
    AddPiece(PreparedPiece),
}

/// A validated `.tqc` or `.tqg` with every cue, asset and group given a new id (FORMAT.md 6.4).
#[derive(Debug, Clone)]
pub struct PreparedPiece {
    pub kind: tqs::Kind,
    pub cues: Vec<Value>,
    /// New assets for the show's manifest (id, asset JSON). Files the show already holds
    /// (same SHA-256 and size) reuse its asset and aren't listed.
    pub assets: Vec<(String, Value)>,
    pub group: Option<Value>,
    /// Audio to move in: (unpacked file, path inside the show folder).
    pub files: Vec<(std::path::PathBuf, String)>,
}

/// Give a validated piece (unpacked in `piece_dir`) new ids for `show` (FORMAT.md 6.4: nothing
/// is merged into something already in the show; a `.tqg`'s cues stay one new group with the
/// file's name and colour; a `.tqc` brings plain cues).
pub fn prepare_piece(
    show: &Manifest,
    show_dir: &Path,
    piece: &Manifest,
    piece_dir: &Path,
) -> PreparedPiece {
    // Audio the show already holds, by content.
    let held: std::collections::HashMap<(&str, u64), &str> = show
        .assets
        .iter()
        .filter(|(_, a)| show_dir.join(&a.path).is_file())
        .map(|(id, a)| ((a.sha256.as_str(), a.bytes), id.as_str()))
        .collect();
    let mut asset_map = std::collections::HashMap::new();
    let mut assets = Vec::new();
    let mut files = Vec::new();
    for (old, a) in &piece.assets {
        if let Some(id) = held.get(&(a.sha256.as_str(), a.bytes)) {
            asset_map.insert(old.clone(), id.to_string());
            continue;
        }
        let id = new_id();
        let ext = Path::new(&a.path)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let rel = format!("audio/{id}.{ext}");
        files.push((piece_dir.join(&a.path), rel.clone()));
        assets.push((
            id.clone(),
            json!({"path": rel, "sha256": a.sha256, "bytes": a.bytes}),
        ));
        asset_map.insert(old.clone(), id);
    }
    let group = (piece.kind == tqs::Kind::Group)
        .then(|| piece.groups.first())
        .flatten()
        .map(|g| json!({"id": format!("g{}", &new_id()[..10]), "name": g.name, "color": g.color}));
    let gid = group.as_ref().map(|g| g["id"].clone());
    let cues = piece
        .cues
        .iter()
        .map(|c| {
            let mut v = serde_json::to_value(c).expect("a cue serialises");
            v["id"] = json!(new_id());
            v["asset"] = match c.asset.as_ref().and_then(|a| asset_map.get(a)) {
                Some(id) => json!(id),
                None => Value::Null,
            };
            match &gid {
                Some(g) => v["group"] = g.clone(),
                None => {
                    v.as_object_mut().expect("cue").remove("group");
                }
            }
            v
        })
        .collect();
    PreparedPiece {
        kind: piece.kind,
        cues,
        assets,
        group,
        files,
    }
}

/// Most a group name may be (FORMAT.md 2.7).
pub const GROUP_NAME_MAX: usize = 60;

fn group_fields(name: &str, color: &str) -> Result<(String, String), String> {
    let name: String = name.trim().chars().take(GROUP_NAME_MAX).collect();
    if name.is_empty() {
        return Err("Give the group a name.".into());
    }
    let color = color.trim().to_lowercase();
    let ok = color.len() == 7
        && color.starts_with('#')
        && color[1..].bytes().all(|b| b.is_ascii_hexdigit());
    if !ok {
        return Err("Choose a colour for the group.".into());
    }
    Ok((name, color))
}

fn members(cues: &[Value], group: &str) -> Vec<usize> {
    (0..cues.len())
        .filter(|&i| group_of(cues.get(i)) == Some(group))
        .collect()
}

/// Move `ids` (in list order) so they sit before or after `anchor`, as the web's `moveCues`.
fn move_block(
    cues: &mut Vec<Value>,
    ids: &[String],
    anchor: &str,
    after: bool,
) -> Result<(), String> {
    let at = cue_index(cues, anchor)? + usize::from(after);
    for id in ids {
        cue_index(cues, id)?;
    }
    let moving = |c: &Value| {
        c["id"]
            .as_str()
            .is_some_and(|id| ids.iter().any(|x| x == id))
    };
    let shift = cues[..at].iter().filter(|c| moving(c)).count();
    let (block, mut rest): (Vec<Value>, Vec<Value>) = cues.drain(..).partition(|c| moving(c));
    let tail = rest.split_off(at - shift);
    rest.extend(block);
    rest.extend(tail);
    *cues = rest;
    Ok(())
}

fn set_group(cue: &mut Value, group: Option<&str>) {
    match group {
        Some(g) => cue["group"] = json!(g),
        None => {
            cue.as_object_mut().expect("cue").remove("group");
        }
    }
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
            Edit::CreateGroup { ids, name, color } => {
                let (name, color) = group_fields(&name, &color)?;
                let groups = obj
                    .get("groups")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let cues = obj["cues"].as_array_mut().expect("cues");
                let mut idx = ids
                    .iter()
                    .map(|id| cue_index(cues, id))
                    .collect::<Result<Vec<_>, _>>()?;
                idx.sort_unstable();
                idx.dedup();
                let (Some(&lo), Some(&hi)) = (idx.first(), idx.last()) else {
                    return Err(
                        "Select a cue first. Shift-click another cue to select a range.".into(),
                    );
                };
                // Grouping the middle of a group would split it in two (web `splitsAGroup`).
                for g in &groups {
                    let gid = g["id"].as_str().unwrap_or_default();
                    let m = members(cues, gid);
                    if let (Some(&gs), Some(&ge)) = (m.first(), m.last())
                        && gs < lo
                        && hi < ge
                    {
                        return Err(format!(
                            "Those cues are in the middle of \"{}\". Select from its start or end, or ungroup it first.",
                            g["name"].as_str().unwrap_or("a group")
                        ));
                    }
                }
                let ordered: Vec<String> = idx
                    .iter()
                    .map(|&i| cues[i]["id"].as_str().unwrap_or_default().to_string())
                    .collect();
                move_block(cues, &ordered, &ordered[0], false)?;
                let gid = format!("g{}", &new_id()[..10]);
                for c in cues.iter_mut() {
                    if c["id"]
                        .as_str()
                        .is_some_and(|id| ordered.iter().any(|x| x == id))
                    {
                        set_group(c, Some(&gid));
                    }
                }
                let groups = obj
                    .entry("groups")
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .ok_or("This show's groups are damaged.")?;
                groups.push(json!({"id": gid, "name": name, "color": color}));
                focus = ordered.first().cloned();
            }
            Edit::UpdateGroup { id, name, color } => {
                let (name, color) = group_fields(&name, &color)?;
                let g = obj
                    .get_mut("groups")
                    .and_then(Value::as_array_mut)
                    .and_then(|gs| gs.iter_mut().find(|g| g["id"] == json!(id)))
                    .ok_or("That group is no longer in the show.")?;
                g["name"] = json!(name);
                g["color"] = json!(color);
            }
            Edit::Ungroup { id } => {
                let gs = obj
                    .get_mut("groups")
                    .and_then(Value::as_array_mut)
                    .ok_or("That group is no longer in the show.")?;
                let before = gs.len();
                gs.retain(|g| g["id"] != json!(id));
                if gs.len() == before {
                    return Err("That group is no longer in the show.".into());
                }
                for c in obj["cues"].as_array_mut().expect("cues") {
                    if group_of(Some(c)) == Some(id.as_str()) {
                        set_group(c, None);
                    }
                }
            }
            Edit::MoveCues {
                ids,
                anchor,
                after,
                group,
            } => {
                let cues = obj["cues"].as_array_mut().expect("cues");
                let mut idx = ids
                    .iter()
                    .map(|id| cue_index(cues, id))
                    .collect::<Result<Vec<_>, _>>()?;
                idx.sort_unstable();
                idx.dedup();
                let ordered: Vec<String> = idx
                    .iter()
                    .map(|&i| cues[i]["id"].as_str().unwrap_or_default().to_string())
                    .collect();
                if let Some(Some(g)) = &group
                    && members(cues, g).is_empty()
                {
                    return Err("That group is no longer in the show.".into());
                }
                move_block(cues, &ordered, &anchor, after)?;
                if let Some(g) = group {
                    for c in cues.iter_mut() {
                        if c["id"]
                            .as_str()
                            .is_some_and(|id| ordered.iter().any(|x| x == id))
                        {
                            set_group(c, g.as_deref());
                        }
                    }
                }
                focus = ordered.first().cloned();
            }
            Edit::SetGroup { id, group } => {
                let cues = obj["cues"].as_array_mut().expect("cues");
                let i = cue_index(cues, &id)?;
                let own = group_of(cues.get(i)).map(String::from);
                if own == group {
                    return Ok((manifest.clone(), Some(id)));
                }
                match &group {
                    Some(to) => {
                        let m = members(cues, to);
                        let last = *m.last().ok_or("That group is no longer in the show.")?;
                        let anchor = cues[last]["id"].as_str().unwrap_or_default().to_string();
                        move_block(cues, std::slice::from_ref(&id), &anchor, true)?;
                    }
                    None => {
                        let m = members(cues, own.as_deref().unwrap_or_default());
                        if m.first() != Some(&i) {
                            let last = *m.last().expect("the cue is a member");
                            let anchor = cues[last]["id"].as_str().unwrap_or_default().to_string();
                            move_block(cues, std::slice::from_ref(&id), &anchor, true)?;
                        }
                    }
                }
                let i = cue_index(cues, &id)?;
                set_group(&mut cues[i], group.as_deref());
                focus = Some(id);
            }
            Edit::AddPiece(p) => {
                let assets = obj["assets"].as_object_mut().expect("assets");
                for (id, a) in p.assets {
                    assets.insert(id, a);
                }
                if let Some(g) = p.group {
                    obj.entry("groups")
                        .or_insert_with(|| json!([]))
                        .as_array_mut()
                        .ok_or("This show's groups are damaged.")?
                        .push(g);
                }
                let cues = obj["cues"].as_array_mut().expect("cues");
                focus = p
                    .cues
                    .first()
                    .and_then(|c| c["id"].as_str())
                    .map(String::from);
                cues.extend(p.cues);
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

    fn ap(m: &Manifest, e: Edit) -> Manifest {
        apply(m, e).unwrap().0
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn creating_a_group_pulls_its_cues_together_and_keeps_the_rest_in_order() {
        let m = grouped_show();
        let (m, focus) = apply(
            &m,
            Edit::CreateGroup {
                ids: ids(&["g07", "g01", "g05"]),
                name: "  Scene 2  ".into(),
                color: "#C98A14".into(),
            },
        )
        .unwrap();
        let g = m.groups.iter().find(|g| g.name == "Scene 2").unwrap();
        assert_eq!(g.color, "#c98a14", "lowercased");
        assert!(g.id.len() <= 64 && g.id.bytes().all(|b| b.is_ascii_alphanumeric()));
        let gid = g.id.clone();
        assert_eq!(
            layout(&m),
            [
                format!("g01:{gid}"),
                format!("g05:{gid}"),
                format!("g07:{gid}"),
                "g02:act1".into(),
                "g03:act1".into(),
                "g04:act1".into(),
                "g06:storm".into(),
            ]
        );
        assert_eq!(focus.as_deref(), Some("g01"));
    }

    #[test]
    fn grouping_the_middle_of_a_group_is_refused() {
        let e = apply(
            &grouped_show(),
            Edit::CreateGroup {
                ids: ids(&["g03"]),
                name: "Inner".into(),
                color: "#3e7bd6".into(),
            },
        )
        .unwrap_err();
        assert!(e.contains("in the middle of"), "{e}");
        // Its start or end is fine: those cues leave act1, which stays consecutive.
        let m = ap(
            &grouped_show(),
            Edit::CreateGroup {
                ids: ids(&["g04", "g05"]),
                name: "Tail".into(),
                color: "#3e7bd6".into(),
            },
        );
        let l = layout(&m);
        assert_eq!(&l[..3], ["g01", "g02:act1", "g03:act1"]);
        assert!(
            l[3].starts_with("g04:g") && l[4].starts_with("g05:g"),
            "{l:?}"
        );
    }

    #[test]
    fn group_names_and_colours_are_checked() {
        let e = |name: &str, color: &str| {
            apply(
                &grouped_show(),
                Edit::UpdateGroup {
                    id: "act1".into(),
                    name: name.into(),
                    color: color.into(),
                },
            )
        };
        assert!(e("   ", "#3e7bd6").is_err());
        assert!(e("Act", "blue").is_err());
        let m = e(&"x".repeat(80), "#1F9E9A").unwrap().0;
        let g = m.groups.iter().find(|g| g.id == "act1").unwrap();
        assert_eq!(g.name.len(), GROUP_NAME_MAX);
        assert_eq!(g.color, "#1f9e9a");
    }

    #[test]
    fn ungrouping_keeps_the_cues_where_they_are() {
        let m = ap(&grouped_show(), Edit::Ungroup { id: "act1".into() });
        assert_eq!(
            layout(&m),
            ["g01", "g02", "g03", "g04", "g05", "g06:storm", "g07:storm"]
        );
        assert_eq!(m.groups.len(), 1);
        assert!(apply(&m, Edit::Ungroup { id: "act1".into() }).is_err());
    }

    #[test]
    fn dragging_a_whole_group_moves_its_cues_and_keeps_membership() {
        let m = ap(
            &grouped_show(),
            Edit::MoveCues {
                ids: ids(&["g06", "g07"]),
                anchor: "g01".into(),
                after: false,
                group: None,
            },
        );
        assert_eq!(
            layout(&m),
            [
                "g06:storm",
                "g07:storm",
                "g01",
                "g02:act1",
                "g03:act1",
                "g04:act1",
                "g05"
            ]
        );
    }

    #[test]
    fn dragging_a_cue_sets_the_group_it_lands_in() {
        // Onto act1's middle row: joins act1.
        let m = ap(
            &grouped_show(),
            Edit::MoveCues {
                ids: ids(&["g01"]),
                anchor: "g03".into(),
                after: true,
                group: Some(Some("act1".into())),
            },
        );
        assert_eq!(
            layout(&m),
            [
                "g02:act1",
                "g03:act1",
                "g01:act1",
                "g04:act1",
                "g05",
                "g06:storm",
                "g07:storm"
            ]
        );
        // To the end (empty space): leaves its group.
        let m = ap(
            &grouped_show(),
            Edit::MoveCues {
                ids: ids(&["g02"]),
                anchor: "g07".into(),
                after: true,
                group: Some(None),
            },
        );
        assert_eq!(
            layout(&m),
            [
                "g01",
                "g03:act1",
                "g04:act1",
                "g05",
                "g06:storm",
                "g07:storm",
                "g02"
            ]
        );
    }

    #[test]
    fn inspector_group_field_joins_at_the_end_and_leaves_below() {
        let m = ap(
            &grouped_show(),
            Edit::SetGroup {
                id: "g01".into(),
                group: Some("storm".into()),
            },
        );
        assert_eq!(
            layout(&m),
            [
                "g02:act1",
                "g03:act1",
                "g04:act1",
                "g05",
                "g06:storm",
                "g07:storm",
                "g01:storm"
            ]
        );
        // Leaving from the middle: just below the group.
        let m = ap(
            &grouped_show(),
            Edit::SetGroup {
                id: "g03".into(),
                group: None,
            },
        );
        assert_eq!(
            layout(&m),
            [
                "g01",
                "g02:act1",
                "g04:act1",
                "g03",
                "g05",
                "g06:storm",
                "g07:storm"
            ]
        );
        // Leaving as the first cue: stays put.
        let m = ap(
            &grouped_show(),
            Edit::SetGroup {
                id: "g02".into(),
                group: None,
            },
        );
        assert_eq!(
            layout(&m),
            [
                "g01",
                "g02",
                "g03:act1",
                "g04:act1",
                "g05",
                "g06:storm",
                "g07:storm"
            ]
        );
    }

    /// Fixture 06 unpacked into `dir/show`, and `piece` of it written and read back into `dir/piece`.
    fn piece_of(
        dir: &Path,
        piece: tqs::Piece,
    ) -> (Manifest, std::path::PathBuf, Manifest, std::path::PathBuf) {
        let show_dir = dir.join("show");
        let (show, _) = tqs::read_package(
            &fixture("valid/06-groups.tqs"),
            &show_dir,
            tqs::Limits::default(),
        )
        .unwrap();
        let file = dir.join("piece.zip");
        tqs::write_piece(&show_dir, &file, &piece).unwrap();
        let piece_dir = dir.join("piece");
        let (p, _) = tqs::read_package_of(
            &file,
            &piece_dir,
            tqs::Limits::default(),
            &tqs::Kind::ALL,
            &mut |_| {},
        )
        .unwrap();
        (show, show_dir, p, piece_dir)
    }

    #[test]
    fn a_tqg_comes_in_as_a_new_group_at_the_end_with_new_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let (show, show_dir, piece, piece_dir) =
            piece_of(tmp.path(), tqs::Piece::Group("storm".into()));
        let p = prepare_piece(&show, &show_dir, &piece, &piece_dir);
        assert!(
            p.files.is_empty() && p.assets.is_empty(),
            "the show already holds this audio: reused, not copied"
        );
        let (m, focus) = apply(&show, Edit::AddPiece(p)).unwrap();
        assert_eq!(m.cues.len(), 9);
        assert_eq!(m.groups.len(), 3);
        let storms: Vec<_> = m.groups.iter().filter(|g| g.name == "Storm").collect();
        assert_eq!(storms.len(), 2, "never merged into the existing group");
        let new = storms.iter().find(|g| g.id != "storm").unwrap();
        assert_eq!(
            &m.cues[7..]
                .iter()
                .map(|c| c.group.as_deref())
                .collect::<Vec<_>>(),
            &[Some(new.id.as_str()); 2]
        );
        assert_eq!(m.cues[7].name, "Thunder");
        assert_eq!(
            m.cues[7].asset, m.cues[5].asset,
            "shares the asset it already had"
        );
        assert!(
            m.cues[7..].iter().all(|c| !c.id.starts_with('g')),
            "new cue ids"
        );
        assert_eq!(focus.as_deref(), Some(m.cues[7].id.as_str()));
        assert_eq!(
            &layout(&m)[..7],
            &layout(&show)[..],
            "the rest of the show is untouched"
        );
    }

    #[test]
    fn a_tqc_brings_plain_cues_and_copies_audio_the_show_lacks() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, _, piece, piece_dir) = piece_of(
            tmp.path(),
            tqs::Piece::Cues(vec!["g02".into(), "g06".into()]),
        );
        let empty_dir = tmp.path().join("empty");
        std::fs::create_dir_all(&empty_dir).unwrap();
        let empty = empty_show("S");
        let p = prepare_piece(&empty, &empty_dir, &piece, &piece_dir);
        assert_eq!(p.files.len(), p.assets.len());
        assert!(!p.files.is_empty());
        assert!(p.group.is_none());
        for (from, rel) in &p.files {
            assert!(from.is_file());
            assert!(rel.starts_with("audio/") && !rel.contains(".."));
        }
        let (m, _) = apply(&empty, Edit::AddPiece(p)).unwrap();
        assert_eq!(m.cues.len(), 2);
        assert!(m.groups.is_empty() && m.cues.iter().all(|c| c.group.is_none()));
        assert!(
            m.cues
                .iter()
                .all(|c| c.asset.as_ref().is_some_and(|a| m.assets.contains_key(a)))
        );
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
