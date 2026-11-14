//! What the engine tells the UI: a full snapshot whenever something changes, positions while
//! anything runs, and notices (one-line messages for a toast).

use serde::Serialize;

/// Per-cue audio preparation (W4: the operator can always tell ready from not yet).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum PrepState {
    /// Silence and fade-out cues: nothing to prepare.
    NotAudio,
    Pending,
    Decoding,
    Ready,
    /// The cue has no audio file, or the file isn't there.
    Missing,
    /// The file is there but can't be decoded. The reason is for the tooltip/log.
    Failed(String),
}

impl PrepState {
    pub fn is_settled(&self) -> bool {
        !matches!(self, PrepState::Pending | PrepState::Decoding)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineStatus {
    /// Output is running.
    Ok,
    /// The output device reported it went away. Press Restart audio engine.
    DeviceLost,
    /// Restart audio engine is in progress; commands are refused.
    Restarting,
    /// No output could be opened at all.
    NoOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    /// GO was pressed but the audio is still decoding; it starts the moment it's ready
    /// unless Stop all comes first (W1).
    Starting,
    /// In its pre-wait.
    Waiting,
    Running,
    /// Being stopped with a fade.
    Fading,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunningCue {
    pub index: usize,
    pub id: String,
    pub phase: RunPhase,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CuePrep {
    #[serde(flatten)]
    pub state: PrepState,
    /// Played length in seconds, once known.
    pub length: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Snapshot {
    /// Increases with every change, so the UI can ignore stale copies.
    pub revision: u64,
    pub status: EngineStatus,
    /// Shown with `DeviceLost` / `NoOutput`: what went wrong, in a sentence.
    pub status_message: Option<String>,
    pub paused: bool,
    /// Index of the standby cue; `cue_count` means "end of the list".
    pub standby: usize,
    pub cue_count: usize,
    pub running: Vec<RunningCue>,
    /// Cue index being auditioned in the trim editor, if any.
    pub audition: Option<usize>,
    /// One entry per cue, in show order.
    pub prep: Vec<CuePrep>,
    /// Audio files settled (ready, missing or failed) and the total. Equal when loaded.
    pub prep_done: usize,
    pub prep_total: usize,
    /// Bytes of decoded audio held in memory.
    pub decoded_bytes: u64,
    /// Master volume, linear 0..=1.
    pub master: f64,
}

impl Snapshot {
    pub fn empty() -> Self {
        Self {
            revision: 0,
            status: EngineStatus::Ok,
            status_message: None,
            paused: false,
            standby: 0,
            cue_count: 0,
            running: Vec::new(),
            audition: None,
            prep: Vec::new(),
            prep_done: 0,
            prep_total: 0,
            decoded_bytes: 0,
            master: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Position {
    pub index: usize,
    pub id: String,
    /// Seconds since the cue's audio started (0 while waiting).
    pub elapsed: f64,
    /// Seconds left, or `None` for a looping cue.
    pub remaining: Option<f64>,
    pub length: f64,
    /// A silence or fade-out cue (the strip shows it only when no audio is running).
    pub control: bool,
    /// The trim editor's audition, not a cue in the show. `source_time` is the playhead in
    /// seconds into the original file.
    pub audition: bool,
    pub source_time: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    State(Snapshot),
    Positions(Vec<Position>),
    Notice(Notice),
}
