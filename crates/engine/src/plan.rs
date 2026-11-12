//! What the engine is told to play. The app builds a `ShowPlan` from the manifest; the engine
//! keeps its own copy and decides what every command means from it.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CueKind {
    Audio,
    Silence,
    Fadeout,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanCue {
    pub id: String,
    pub name: String,
    pub kind: CueKind,
    /// Resolved audio file, or `None` when the cue's audio is missing.
    pub audio: Option<PathBuf>,
    /// Linear amplitude, 0..=2.
    pub volume: f64,
    pub fade_in: f64,
    pub fade_out: f64,
    pub pre_wait: f64,
    pub duration: f64,
    pub looping: bool,
    pub continue_after: bool,
    pub trim_start: f64,
    pub trim_end: Option<f64>,
    pub cuts: Vec<[f64; 2]>,
}

impl PlanCue {
    /// An audio cue with everything else at its default. Handy in tests.
    pub fn audio(id: &str, path: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            name: id.into(),
            kind: CueKind::Audio,
            audio: Some(path.into()),
            volume: 1.0,
            fade_in: 0.0,
            fade_out: 0.0,
            pre_wait: 0.0,
            duration: 0.0,
            looping: false,
            continue_after: false,
            trim_start: 0.0,
            trim_end: None,
            cuts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShowPlan {
    pub cues: Vec<PlanCue>,
    /// Where standby starts (clamped to `cues.len()`, which means "end of the list").
    pub standby: usize,
}
