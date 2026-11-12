//! `manifest.json`: types, defaults, clamping and validation (FORMAT.md section 2).
//!
//! Parsing goes through `serde_json::Value` and a hand-written normaliser rather than
//! `#[derive(Deserialize)]`, because the rules are "wrong type: reject, out of range: clamp,
//! missing: default" with a user-facing message for each, which serde attributes can't say.
//! Unknown fields are kept in `extra` and written back out (FORMAT.md 2.6).

use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::{Result, TqsError, Warning};
use crate::{MAX_CUES, MAX_CUTS};

pub const FORMAT: &str = "thorncue.show";
pub const FORMAT_VERSION: u64 = 1;
pub const AUDIO_EXTS: [&str; 7] = ["mp3", "wav", "ogg", "flac", "m4a", "aiff", "aif"];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Manifest {
    pub format: String,
    pub format_version: u64,
    pub generator: Generator,
    pub exported_at: String,
    pub show: Show,
    pub assets: IndexMap<String, Asset>,
    pub cues: Vec<Cue>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Generator {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Show {
    pub name: String,
    pub author: String,
    pub notes: String,
    pub source: Option<Source>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Source {
    pub kind: String,
    pub server: String,
    pub code: String,
    pub revision: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Asset {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CueType {
    Audio,
    Silence,
    Fadeout,
}

impl CueType {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "audio" => Some(Self::Audio),
            "silence" => Some(Self::Silence),
            "fadeout" => Some(Self::Fadeout),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Silence => "silence",
            Self::Fadeout => "fadeout",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Continue {
    None,
    AfterEnd,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Cue {
    pub id: String,
    #[serde(rename = "type")]
    pub cue_type: CueType,
    pub name: String,
    pub asset: Option<String>,
    pub volume: f64,
    pub fade_in: f64,
    pub fade_out: f64,
    pub pre_wait: f64,
    pub duration: f64,
    #[serde(rename = "loop")]
    pub looping: bool,
    #[serde(rename = "continue")]
    pub continue_mode: Continue,
    pub trim_start: f64,
    pub trim_end: Option<f64>,
    pub cuts: Vec<[f64; 2]>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Manifest {
    /// Parse and normalise `manifest.json` bytes (UTF-8, no BOM).
    pub fn from_slice(bytes: &[u8]) -> Result<(Manifest, Vec<Warning>)> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| TqsError::invalid("manifest.json isn't valid UTF-8 text."))?;
        if text.starts_with('\u{feff}') {
            return Err(TqsError::invalid(
                "manifest.json starts with a byte-order mark. Save it as UTF-8 without one.",
            ));
        }
        let raw: Value = serde_json::from_str(text).map_err(|e| {
            TqsError::invalid(format!(
                "manifest.json isn't valid JSON (line {}, column {}).",
                e.line(),
                e.column()
            ))
        })?;
        normalise_manifest(&raw)
    }

    /// Pretty JSON, as written into packages and library folders.
    pub fn to_json_pretty(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("a manifest always serialises")
    }

    /// Total bytes of audio in the show.
    pub fn audio_bytes(&self) -> u64 {
        self.assets.values().map(|a| a.bytes).sum()
    }
}

// ---------------------------------------------------------------------------
// Field helpers (tqs_ref.py `_num`, `_str`)
// ---------------------------------------------------------------------------

/// Round to 4 decimal places, as the reference reader does (FORMAT.md 2.4 "writers SHOULD").
fn round4(v: f64) -> f64 {
    let r = (v * 10_000.0).round() / 10_000.0;
    if r == 0.0 { 0.0 } else { r }
}

fn num(
    cue: &str,
    field: &str,
    value: Option<&Value>,
    default: f64,
    lo: f64,
    hi: Option<f64>,
) -> Result<f64> {
    let v = match value {
        None | Some(Value::Null) => return Ok(default),
        Some(Value::Number(n)) => n.as_f64().ok_or_else(|| {
            TqsError::invalid(format!("Cue \"{cue}\": {field} must be a number."))
        })?,
        Some(_) => {
            return Err(TqsError::invalid(format!(
                "Cue \"{cue}\": {field} must be a number."
            )));
        }
    };
    if !v.is_finite() {
        return Err(TqsError::invalid(format!(
            "Cue \"{cue}\": {field} must be a finite number."
        )));
    }
    let mut v = v.max(lo);
    if let Some(hi) = hi {
        v = v.min(hi);
    }
    Ok(round4(v))
}

fn text(value: Option<&Value>, default: &str, max_chars: usize, what: &str) -> Result<String> {
    match value {
        None | Some(Value::Null) => Ok(default.to_string()),
        Some(Value::String(s)) => {
            let t: String = s.trim().chars().take(max_chars).collect();
            Ok(if t.is_empty() { default.to_string() } else { t })
        }
        Some(_) => Err(TqsError::invalid(format!("{what} must be text."))),
    }
}

pub(crate) fn is_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn is_sha(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `audio/<id>.<ext>` with a known lowercase extension. Returns the id.
pub(crate) fn asset_path_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("audio/")?;
    let (id, ext) = rest.rsplit_once('.')?;
    (is_id(id) && AUDIO_EXTS.contains(&ext)).then_some(id)
}

fn extra_fields(obj: &Map<String, Value>, known: &[&str]) -> Map<String, Value> {
    obj.iter()
        .filter(|(k, _)| !known.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn describe(v: &Value) -> String {
    match v {
        Value::String(s) => format!("\"{s}\""),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Cuts and cues
// ---------------------------------------------------------------------------

/// Drop invalid pairs, sort, merge overlaps (FORMAT.md 2.5). A non-list is a rejection.
pub fn normalise_cuts(raw: Option<&Value>, cue: &str) -> Result<Vec<[f64; 2]>> {
    let list = match raw {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(_) => {
            return Err(TqsError::invalid(format!(
                "Cue \"{cue}\": cuts must be a list."
            )));
        }
    };
    let mut pairs: Vec<[f64; 2]> = list
        .iter()
        .take(MAX_CUTS)
        .filter_map(|p| {
            let p = p.as_array()?;
            if p.len() != 2 {
                return None;
            }
            let a = p[0].as_number()?.as_f64()?;
            let b = p[1].as_number()?.as_f64()?;
            (a.is_finite() && b.is_finite() && a >= 0.0 && b > a).then(|| [round4(a), round4(b)])
        })
        .collect();
    pairs.sort_by(|x, y| x[0].total_cmp(&y[0]).then(x[1].total_cmp(&y[1])));
    let mut merged: Vec<[f64; 2]> = Vec::with_capacity(pairs.len());
    for [a, b] in pairs {
        match merged.last_mut() {
            Some(last) if a <= last[1] => last[1] = last[1].max(b),
            _ => merged.push([a, b]),
        }
    }
    Ok(merged)
}

const CUE_FIELDS: [&str; 14] = [
    "id",
    "type",
    "name",
    "asset",
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

fn normalise_cue(
    raw: &Value,
    assets: &IndexMap<String, Asset>,
    seen: &mut std::collections::HashSet<String>,
) -> Result<Cue> {
    let obj = raw
        .as_object()
        .ok_or_else(|| TqsError::invalid("Every cue in manifest.json must be an object."))?;
    let id = match obj.get("id") {
        Some(Value::String(s)) if is_id(s) => s.clone(),
        Some(v) => {
            return Err(TqsError::invalid(format!(
                "Cue id {} is not valid.",
                describe(v)
            )));
        }
        None => return Err(TqsError::invalid("A cue has no id.")),
    };
    if !seen.insert(id.clone()) {
        return Err(TqsError::invalid(format!(
            "Two cues share the id \"{id}\"."
        )));
    }
    let cue_type = obj
        .get("type")
        .and_then(Value::as_str)
        .and_then(CueType::parse)
        .ok_or_else(|| {
            let t = obj.get("type").map(describe).unwrap_or_else(|| "nothing".into());
            TqsError::invalid(format!(
                "Cue {id} has type {t}, which this version of ThornCue doesn't know. Update ThornCue to open this show."
            ))
        })?;
    let name = text(
        obj.get("name"),
        "Untitled cue",
        100,
        &format!("Cue {id}: name"),
    )?;

    let continue_mode = match obj.get("continue") {
        None | Some(Value::Null) => Continue::None,
        Some(Value::String(s)) if s == "none" => Continue::None,
        Some(Value::String(s)) if s == "after_end" => Continue::AfterEnd,
        Some(v) => {
            return Err(TqsError::invalid(format!(
                "Cue \"{name}\" uses continue mode {}, which this version of ThornCue doesn't know. Update ThornCue to open this show.",
                describe(v)
            )));
        }
    };

    let asset = match obj.get("asset") {
        None | Some(Value::Null) => None,
        Some(Value::String(a)) => {
            if cue_type != CueType::Audio {
                return Err(TqsError::invalid(format!(
                    "Cue \"{name}\" is a {} cue but has an audio file.",
                    cue_type.as_str()
                )));
            }
            if !assets.contains_key(a) {
                return Err(TqsError::invalid(format!(
                    "Cue \"{name}\" points to audio \"{a}\", which isn't in the package."
                )));
            }
            Some(a.clone())
        }
        Some(_) => {
            return Err(TqsError::invalid(format!(
                "Cue \"{name}\": asset must be text or null."
            )));
        }
    };

    let looping = match obj.get("loop") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            return Err(TqsError::invalid(format!(
                "Cue \"{name}\": loop must be true or false."
            )));
        }
    };

    let trim_start = num(&name, "trim_start", obj.get("trim_start"), 0.0, 0.0, None)?;
    let trim_end = match obj.get("trim_end") {
        None | Some(Value::Null) => None,
        v => Some(num(&name, "trim_end", v, 0.0, 0.0, None)?),
    }
    .filter(|&e| e > trim_start);

    let is_audio = cue_type == CueType::Audio;
    Ok(Cue {
        volume: num(&name, "volume", obj.get("volume"), 1.0, 0.0, Some(2.0))?,
        fade_in: num(&name, "fade_in", obj.get("fade_in"), 0.0, 0.0, Some(600.0))?,
        fade_out: num(
            &name,
            "fade_out",
            obj.get("fade_out"),
            0.0,
            0.0,
            Some(600.0),
        )?,
        pre_wait: num(
            &name,
            "pre_wait",
            obj.get("pre_wait"),
            0.0,
            0.0,
            Some(3600.0),
        )?,
        duration: if is_audio {
            0.0
        } else {
            num(
                &name,
                "duration",
                obj.get("duration"),
                0.0,
                0.0,
                Some(3600.0),
            )?
        },
        looping: looping && is_audio,
        cuts: normalise_cuts(obj.get("cuts"), &name)?,
        extra: extra_fields(obj, &CUE_FIELDS),
        id,
        cue_type,
        name,
        asset,
        continue_mode,
        trim_start,
        trim_end,
    })
}

// ---------------------------------------------------------------------------
// The whole manifest
// ---------------------------------------------------------------------------

/// Validate and normalise a parsed manifest (tqs_ref.py `normalise_manifest`).
pub fn normalise_manifest(raw: &Value) -> Result<(Manifest, Vec<Warning>)> {
    let mut warnings = Vec::new();
    let obj = raw
        .as_object()
        .ok_or_else(|| TqsError::invalid("manifest.json must be a JSON object."))?;
    if obj.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(TqsError::invalid(
            "This isn't a ThornCue show (manifest.json has the wrong format name).",
        ));
    }
    let version = match obj.get("format_version") {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_i64().unwrap_or(i64::MAX),
        _ => {
            return Err(TqsError::invalid(
                "manifest.json has no valid format_version.",
            ));
        }
    };
    if version > FORMAT_VERSION as i64 {
        return Err(TqsError::invalid(
            "This show was made by a newer version of ThornCue. Update ThornCue to open it.",
        ));
    }
    if version < 1 {
        return Err(TqsError::invalid(format!(
            "Unknown format_version {version}."
        )));
    }

    let show_obj = obj
        .get("show")
        .and_then(Value::as_object)
        .ok_or_else(|| TqsError::invalid("manifest.json has no show section."))?;
    let source = match show_obj.get("source") {
        None | Some(Value::Null) => None,
        Some(Value::Object(src)) => {
            if src.get("kind").and_then(Value::as_str) != Some("thorncue-web") {
                let kind = src
                    .get("kind")
                    .map(describe)
                    .unwrap_or_else(|| "nothing".into());
                warnings.push(Warning(format!(
                    "Ignoring unknown show source kind {kind}."
                )));
                None
            } else {
                let field = |k: &str| -> Result<String> {
                    match src.get(k) {
                        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
                        _ => Err(TqsError::invalid(format!("show.source.{k} is missing."))),
                    }
                };
                Some(Source {
                    kind: "thorncue-web".into(),
                    server: field("server")?.trim_end_matches('/').to_string(),
                    code: field("code")?,
                    revision: field("revision")?,
                    extra: extra_fields(src, &["kind", "server", "code", "revision"]),
                })
            }
        }
        Some(_) => return Err(TqsError::invalid("show.source must be an object or null.")),
    };
    let show = Show {
        name: text(show_obj.get("name"), "Untitled show", 100, "Show name")?,
        author: text(show_obj.get("author"), "", 100, "Author")?,
        notes: text(show_obj.get("notes"), "", 2000, "Notes")?,
        source,
        extra: extra_fields(show_obj, &["name", "author", "notes", "source"]),
    };

    let mut assets = IndexMap::new();
    match obj.get("assets") {
        None | Some(Value::Null) => {}
        Some(Value::Object(map)) => {
            for (aid, a) in map {
                if !is_id(aid) {
                    return Err(TqsError::invalid(format!(
                        "Audio id \"{aid}\" is not valid."
                    )));
                }
                let a = a
                    .as_object()
                    .ok_or_else(|| TqsError::invalid(format!("Audio {aid} must be an object.")))?;
                let path = a.get("path").and_then(Value::as_str);
                if path.and_then(asset_path_id) != Some(aid.as_str()) {
                    let shown = a
                        .get("path")
                        .map(describe)
                        .unwrap_or_else(|| "nothing".into());
                    return Err(TqsError::invalid(format!(
                        "Audio {aid} has an invalid path {shown}."
                    )));
                }
                let sha = a
                    .get("sha256")
                    .and_then(Value::as_str)
                    .filter(|s| is_sha(s));
                let Some(sha) = sha else {
                    return Err(TqsError::invalid(format!(
                        "Audio {aid} has an invalid sha256."
                    )));
                };
                let bytes = a.get("bytes").and_then(Value::as_u64).filter(|&b| b >= 1);
                let Some(bytes) = bytes else {
                    return Err(TqsError::invalid(format!(
                        "Audio {aid} has an invalid size."
                    )));
                };
                assets.insert(
                    aid.clone(),
                    Asset {
                        path: path.unwrap_or_default().to_string(),
                        sha256: sha.to_string(),
                        bytes,
                        extra: extra_fields(a, &["path", "sha256", "bytes"]),
                    },
                );
            }
        }
        Some(_) => return Err(TqsError::invalid("assets must be an object.")),
    }

    let cues_raw: &[Value] = match obj.get("cues") {
        None | Some(Value::Null) => &[],
        Some(Value::Array(a)) => a,
        Some(_) => return Err(TqsError::invalid("cues must be a list.")),
    };
    if cues_raw.len() > MAX_CUES {
        return Err(TqsError::invalid(format!(
            "This show has {} cues; the limit is {MAX_CUES}.",
            cues_raw.len()
        )));
    }
    let mut seen = std::collections::HashSet::new();
    let cues = cues_raw
        .iter()
        .map(|c| normalise_cue(c, &assets, &mut seen))
        .collect::<Result<Vec<_>>>()?;

    let used: std::collections::HashSet<&str> =
        cues.iter().filter_map(|c| c.asset.as_deref()).collect();
    let unused: Vec<String> = assets
        .keys()
        .filter(|k| !used.contains(k.as_str()))
        .cloned()
        .collect();
    for aid in unused {
        warnings.push(Warning(format!(
            "Audio {aid} isn't used by any cue; dropping it."
        )));
        assets.shift_remove(&aid);
    }

    let gen_obj = obj.get("generator").and_then(Value::as_object);
    let gen_field = |k: &str| -> String {
        let s = match gen_obj.and_then(|g| g.get(k)) {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
        };
        s.chars().take(64).collect()
    };

    Ok((
        Manifest {
            format: FORMAT.into(),
            format_version: FORMAT_VERSION,
            generator: Generator {
                name: gen_field("name"),
                version: gen_field("version"),
            },
            exported_at: obj
                .get("exported_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            show,
            assets,
            cues,
            extra: extra_fields(
                obj,
                &[
                    "format",
                    "format_version",
                    "generator",
                    "exported_at",
                    "show",
                    "assets",
                    "cues",
                ],
            ),
        },
        warnings,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cuts_are_filtered_sorted_and_merged() {
        let raw = json!([
            [1.2, 1.4],
            [0.2, 0.6],
            [0.5, 0.8],
            [0.9, 0.9],
            [1.0],
            "x",
            [-1, 0.1],
            [true, 2]
        ]);
        assert_eq!(
            normalise_cuts(Some(&raw), "q").unwrap(),
            vec![[0.2, 0.8], [1.2, 1.4]]
        );
        assert!(normalise_cuts(Some(&json!("nope")), "q").is_err());
    }

    #[test]
    fn only_the_first_fifty_cuts_count() {
        let raw: Vec<Value> = (0..60)
            .map(|i| json!([i as f64 * 2.0, i as f64 * 2.0 + 1.0]))
            .collect();
        assert_eq!(
            normalise_cuts(Some(&Value::Array(raw)), "q").unwrap().len(),
            MAX_CUTS
        );
    }

    #[test]
    fn numbers_clamp_but_wrong_types_reject() {
        let m = |cue: Value| {
            normalise_manifest(&json!({
                "format": FORMAT, "format_version": 1, "show": {"name": "S"}, "cues": [cue]
            }))
        };
        let (man, _) =
            m(json!({"id": "a", "type": "silence", "volume": 9, "duration": -1})).unwrap();
        assert_eq!(man.cues[0].volume, 2.0);
        assert_eq!(man.cues[0].duration, 0.0);
        assert!(m(json!({"id": "a", "type": "silence", "volume": "loud"})).is_err());
        assert!(m(json!({"id": "a", "type": "silence", "loop": 1})).is_err());
        assert!(m(json!({"id": "A", "type": "silence"})).is_err());
    }

    #[test]
    fn version_must_be_an_integer() {
        let r = normalise_manifest(&json!({"format": FORMAT, "format_version": 1.0, "show": {}}));
        assert!(r.is_err());
    }
}
