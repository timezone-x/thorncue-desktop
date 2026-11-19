//! The engine proper. `Core` is synchronous: the engine thread (`thread.rs`) feeds it commands,
//! prep results and ticks; tests drive it directly with a `TestBackend`.
//!
//! Playback rules are FORMAT.md 2.5. Timing lives on two kira clocks:
//! - the *show clock* starts sounds (pre-wait) and times silence and fade-out cues; it is
//!   paused together with the show track, so Pause freezes everything;
//! - the *heartbeat* never pauses and only moves while the device pulls audio.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use kira::clock::{ClockHandle, ClockSpeed, ClockTime};
use kira::sound::PlaybackState;
use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle};
use kira::track::{TrackBuilder, TrackHandle};
use kira::{AudioManager, Decibels, StartTime, Tween};

use crate::backend::EngineBackend;
use crate::edit::EditKey;
use crate::gain::{CueGainBuilder, CueGainHandle, GainPhase};
use crate::outputs::{
    AuditionBlock, AuditionOutput, AuditionRoute, DeviceList, OutputDevice, ShowOutput, ShowRoute,
    audition_route, show_route,
};
use crate::plan::{CueKind, ShowPlan};
use crate::prep::{Job, Loader, MISSING, PrepMsg, PrepPool};
use crate::state::{
    AuditionState, CuePrep, EngineStatus, Event, Notice, OutputsState, Position, PrepState,
    RunPhase, RunningCue, Snapshot,
};

/// Builds the show's audio manager on a device (`None` = the system default). Called on the
/// engine thread at start, on Restart audio engine and when the show output is changed.
pub type ManagerFactory<B> =
    Box<dyn FnMut(Option<&OutputDevice>) -> Result<AudioManager<B>, String> + Send>;

/// Opens the audition output on a device (`None` = the system default, only for "Same as show
/// output"). Answers on the returned channel: the real one opens on its own thread, so the
/// engine thread never waits for a device.
pub type AuditionOpener<A> = Box<
    dyn FnMut(Option<OutputDevice>) -> crossbeam_channel::Receiver<Result<AudioManager<A>, String>>
        + Send,
>;

/// The outputs the engine starts with (from the app's settings and a first device listing).
#[derive(Debug, Clone, Default)]
pub struct OutputSetup {
    pub show: ShowOutput,
    pub audition: AuditionOutput,
    pub devices: DeviceList,
    /// Linear amplitude 0..=1.
    pub audition_volume: f64,
}

/// Everything the UI can ask for. Commands are intents; the engine decides what they mean.
#[derive(Debug, Clone)]
pub enum Command {
    /// A different show: everything stops and its audio is prepared from scratch.
    LoadShow(ShowPlan),
    /// The same show, edited: playback carries on (see `Core::update_plan`).
    UpdatePlan(ShowPlan),
    /// Fire standby and move standby on.
    Go,
    /// Stop everything and cancel every pending start. `panic` skips any stop fade.
    StopAll {
        panic: bool,
    },
    TogglePause,
    SetStandby(usize),
    MoveStandby(i32),
    /// J: fire cue `index` now (standby moves past it).
    JumpAndPlay(usize),
    RestartAudio,
    /// Master volume, linear amplitude 0..=1.
    SetMaster(f64),
    /// A cue's volume changed: takes effect at once if it is playing (as the web).
    SetCueVolume {
        index: usize,
        volume: f64,
    },
    SetOptions(Options),
    /// Waveform peaks of an audio file in the show, for the trim editor. Answered on `reply`
    /// from the prep pool (after the file has decoded, if it hasn't yet).
    Peaks {
        path: PathBuf,
        reply: crate::peaks::PeaksReply,
    },
    /// Play cue `index` from `from` seconds into its file on the audition output, with its
    /// volume and fades as they would be heard at that point (no pre-wait). Only on the
    /// audition output; refused with the reason when auditions are blocked (thorncue-spec#9).
    Audition {
        index: usize,
        from: f64,
    },
    StopAudition,
    /// The operator's output choices (saved per machine by the app).
    SetOutputs {
        show: ShowOutput,
        audition: AuditionOutput,
    },
    /// The devices present now (from the device watcher).
    Devices(DeviceList),
    /// Audition volume, linear amplitude 0..=1, separate from the master.
    SetAuditionVolume(f64),
}

/// Operator settings that change how the transport behaves.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Options {
    /// Ignore a GO within this many milliseconds of the last (0 = off). W6.
    pub go_lockout_ms: u64,
    /// Stop all fades over this many seconds (0 = instant, as the web). W7.
    pub stop_fade: f64,
}

const INSTANT: Tween = Tween {
    start_time: StartTime::Immediate,
    duration: Duration::ZERO,
    easing: kira::Easing::Linear,
};

/// Ticks per second of the show clock. Start times are rounded to this.
const CLOCK_RATE: f64 = 1000.0;

/// Linear amplitude to kira decibels (used for the master). kira treats -60 dB as silence.
pub(crate) fn amplitude_to_db(a: f64) -> Decibels {
    if a <= 0.001 {
        Decibels::SILENCE
    } else {
        Decibels((20.0 * a.log10()) as f32)
    }
}

struct Asset {
    path: PathBuf,
    state: PrepState,
    /// The decoded file, kept so a changed edit only needs rendering, not decoding.
    raw: Option<StaticSoundData>,
    /// Edited audio for each edit of this file in the show (`None` = length 0).
    edits: HashMap<EditKey, Option<StaticSoundData>>,
    /// Edits submitted to the prep pool and not back yet.
    in_flight: Vec<EditKey>,
    /// Peaks asked for before the file finished decoding.
    peaks_waiting: Vec<crate::peaks::PeaksReply>,
    /// Bytes held in memory (decoded source plus rendered cuts).
    bytes: u64,
}

// A handful of these exist at a time; boxing the big variant would buy nothing.
#[allow(clippy::large_enum_variant)]
enum Running {
    Audio {
        index: usize,
        /// The cue's own sub-track; kept so it lives as long as the sound.
        track: TrackHandle,
        sound: StaticSoundHandle,
        gain: CueGainHandle,
        length: f64,
        looping: bool,
        /// Stopped by Stop all, a silence or fade-out cue, or a re-GO: never continues.
        stopped: bool,
        /// Show-clock tick the sound starts at (after its pre-wait).
        start: u64,
        /// Seconds into the edited audio it started from (0, unless it was moved to another
        /// show output mid-cue).
        offset: f64,
    },
    /// Silence and fade-out cues, and audio cues of length 0 (after their pre-wait).
    Timer { index: usize, start: u64, end: u64 },
}

impl Running {
    fn index(&self) -> usize {
        match self {
            Running::Audio { index, .. } | Running::Timer { index, .. } => *index,
        }
    }
}

/// The trim editor's audition: one sound, never part of the show.
struct Audition {
    index: usize,
    track: TrackHandle,
    sound: StaticSoundHandle,
    gain: CueGainHandle,
    /// Source-file segments of the audio being played, in order, and where in it play began.
    segments: Vec<(f64, f64)>,
    offset: f64,
}

impl Audition {
    /// Playhead in source-file seconds.
    fn source_time(&self) -> f64 {
        let mut t = self.offset + self.gain.elapsed();
        for &(s, e) in &self.segments {
            if t <= e - s {
                return s + t;
            }
            t -= e - s;
        }
        self.segments.last().map_or(0.0, |s| s.1)
    }
}

/// Seconds into the edited audio that correspond to `source` seconds into the file. Inside a
/// cut: the start of the next kept segment.
fn edited_time(segments: &[(f64, f64)], source: f64) -> f64 {
    let mut acc = 0.0;
    for &(s, e) in segments {
        if source < s {
            return acc;
        }
        if source < e {
            return acc + (source - s);
        }
        acc += e - s;
    }
    acc
}

/// A GO on a cue whose audio is still decoding (W1).
struct PendingStart {
    index: usize,
}

/// The audition output: its own manager on its own device (thorncue-spec#9), so nothing the
/// show does (Stop all, panic, Restart) touches it.
struct AuditionOut<A: EngineBackend> {
    opener: Option<AuditionOpener<A>>,
    manager: Option<AudioManager<A>>,
    /// Audition volume lives on this track.
    track: Option<TrackHandle>,
    /// The route the manager is open (or opening) for.
    open_for: Option<AuditionRoute>,
    opening: Option<crossbeam_channel::Receiver<Result<AudioManager<A>, String>>>,
    /// The route failed to open or lost its device: not retried until the devices or the
    /// choice change.
    failed: Option<(AuditionRoute, AuditionBlock)>,
}

pub struct Core<B: EngineBackend, A: EngineBackend = B> {
    factory: ManagerFactory<B>,
    manager: Option<AudioManager<B>>,
    /// What the show manager was opened on (`None` = the system default).
    show_device: Option<OutputDevice>,
    show_choice: ShowOutput,
    audition_choice: AuditionOutput,
    devices: DeviceList,
    audition_volume: f64,
    aud: AuditionOut<A>,
    show_track: Option<TrackHandle>,
    show_clock: Option<ClockHandle>,
    /// Ticks only while the device is pulling audio: if it stops, the output is dead.
    heartbeat: Option<ClockHandle>,
    /// Last tick count seen, when it changed, and whether it has moved since the output opened.
    last_beat: (u64, Instant, bool),
    /// Device lost because the clock stood still (not because the backend said so). This kind
    /// clears itself once audio flows steadily again; a reported disconnect stays until Restart.
    lost_by_stall: bool,
    recovering_since: Option<Instant>,
    status: EngineStatus,
    status_message: Option<String>,

    plan: ShowPlan,
    /// Cue index -> asset index (cues sharing a file share one decode).
    cue_asset: Vec<Option<usize>>,
    assets: Vec<Asset>,
    by_path: HashMap<PathBuf, usize>,
    generation: u64,
    prep: PrepPool,

    standby: usize,
    paused: bool,
    master: f64,
    options: Options,
    last_go: Option<Instant>,
    running: Vec<Running>,
    pending: Vec<PendingStart>,
    /// Sounds stopped by us, kept until kira reports them stopped.
    stopping: Vec<(TrackHandle, StaticSoundHandle)>,
    audition: Option<Audition>,
    /// Running cues' phases as last sent: the audio thread changes them (a pre-wait ends, a
    /// fade starts), so `tick` compares and sends a new snapshot when they move.
    last_phases: Vec<(usize, RunPhase)>,

    out: Box<dyn FnMut(Event) + Send>,
    revision: u64,
    dirty: bool,
    last_positions: Instant,
}

impl<B: EngineBackend, A: EngineBackend> Core<B, A> {
    /// A core with no audition output (auditions are blocked until one is chosen and opens).
    pub fn new(
        factory: ManagerFactory<B>,
        loader: Loader,
        out: Box<dyn FnMut(Event) + Send>,
    ) -> Self {
        Self::with_outputs(factory, None, OutputSetup::default(), loader, out)
    }

    pub fn with_outputs(
        mut factory: ManagerFactory<B>,
        audition_opener: Option<AuditionOpener<A>>,
        setup: OutputSetup,
        loader: Loader,
        out: Box<dyn FnMut(Event) + Send>,
    ) -> Self {
        let show_device = show_route(&setup.show, &setup.devices).device;
        let (manager, status, status_message) = match factory(show_device.as_ref()) {
            Ok(m) => (Some(m), EngineStatus::Ok, None),
            Err(e) => {
                tracing::error!("couldn't open audio output: {e}");
                (None, EngineStatus::NoOutput, Some(no_output_message()))
            }
        };
        let mut core = Self {
            factory,
            manager,
            show_device,
            show_choice: setup.show,
            audition_choice: setup.audition,
            devices: setup.devices,
            audition_volume: setup.audition_volume.clamp(0.0, 1.0),
            aud: AuditionOut {
                opener: audition_opener,
                manager: None,
                track: None,
                open_for: None,
                opening: None,
                failed: None,
            },
            show_track: None,
            show_clock: None,
            heartbeat: None,
            last_beat: (0, Instant::now(), false),
            lost_by_stall: false,
            recovering_since: None,
            status,
            status_message,
            plan: ShowPlan::default(),
            cue_asset: Vec::new(),
            assets: Vec::new(),
            by_path: HashMap::new(),
            generation: 0,
            prep: PrepPool::new(2, loader),
            standby: 0,
            paused: false,
            master: 1.0,
            options: Options::default(),
            last_go: None,
            running: Vec::new(),
            pending: Vec::new(),
            stopping: Vec::new(),
            audition: None,
            last_phases: Vec::new(),
            out,
            revision: 0,
            dirty: true,
            last_positions: Instant::now(),
        };
        core.build_mix();
        core.reconcile_audition();
        core
    }

    /// The show track, show clock and heartbeat on the current manager.
    fn build_mix(&mut self) {
        self.show_track = None;
        self.show_clock = None;
        self.heartbeat = None;
        self.last_beat = (0, Instant::now(), false);
        self.lost_by_stall = false;
        self.recovering_since = None;
        let master = amplitude_to_db(self.master);
        let Some(m) = &mut self.manager else { return };
        match m.add_clock(ClockSpeed::TicksPerSecond(1000.0)) {
            Ok(mut c) => {
                c.start();
                self.heartbeat = Some(c);
            }
            Err(e) => tracing::warn!("no heartbeat clock: {e}"),
        }
        let built = m
            .add_clock(ClockSpeed::TicksPerSecond(CLOCK_RATE))
            .map_err(|e| e.to_string())
            .and_then(|clock| {
                m.add_sub_track(TrackBuilder::new().sub_track_capacity(64).volume(master))
                    .map(|t| (clock, t))
                    .map_err(|e| e.to_string())
            });
        match built {
            Ok((mut clock, track)) => {
                clock.start();
                self.show_clock = Some(clock);
                self.show_track = Some(track);
            }
            Err(e) => {
                tracing::error!("couldn't create the show track: {e}");
                self.status = EngineStatus::NoOutput;
                self.status_message = Some(no_output_message());
            }
        }
    }

    pub fn backend_mut(&mut self) -> Option<&mut B> {
        self.manager.as_mut().map(|m| m.backend_mut())
    }

    /// The audition output's backend, while it is open.
    pub fn audition_backend_mut(&mut self) -> Option<&mut A> {
        self.aud.manager.as_mut().map(|m| m.backend_mut())
    }

    pub(crate) fn prep_results(&self) -> crossbeam_channel::Receiver<PrepMsg> {
        self.prep.results.clone()
    }

    fn notice(&mut self, text: impl Into<String>, error: bool) {
        (self.out)(Event::Notice(Notice {
            text: text.into(),
            error,
        }));
    }

    fn q(&self, index: usize) -> String {
        let name = self
            .plan
            .cues
            .get(index)
            .map(|c| c.name.as_str())
            .unwrap_or("");
        format!(
            "{} \"{name}\"",
            tqs::cue_number(index, self.plan.cues.len(), false)
        )
    }

    fn clock_now(&self) -> Option<ClockTime> {
        self.show_clock.as_ref().map(|c| c.time())
    }

    fn edit_key(&self, i: usize) -> EditKey {
        let c = &self.plan.cues[i];
        EditKey::new(c.trim_start, c.trim_end, &c.cuts)
    }

    // -----------------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------------

    pub fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::LoadShow(plan) => self.load_show(plan),
            Command::UpdatePlan(plan) => self.update_plan(plan),
            Command::Go => self.go(),
            Command::StopAll { panic } => self.stop_all(panic),
            Command::TogglePause => self.toggle_pause(),
            Command::SetStandby(i) => self.standby = i.min(self.plan.cues.len()),
            Command::MoveStandby(d) => {
                if !self.plan.cues.is_empty() {
                    let last = self.plan.cues.len() as i64 - 1;
                    self.standby = (self.standby as i64 + d as i64).clamp(0, last) as usize;
                }
            }
            Command::JumpAndPlay(i) => {
                if i < self.plan.cues.len() {
                    self.fire(i);
                } else {
                    let n = self.plan.cues.len();
                    self.notice(
                        format!(
                            "There is no {}. This show has {n} cue{}.",
                            tqs::cue_number(i, n, false),
                            if n == 1 { "" } else { "s" }
                        ),
                        true,
                    );
                }
            }
            Command::RestartAudio => self.restart(),
            Command::SetMaster(v) => {
                self.master = v.clamp(0.0, 1.0);
                if let Some(t) = &mut self.show_track {
                    // A short ramp so dragging the slider doesn't click.
                    t.set_volume(
                        amplitude_to_db(self.master),
                        Tween {
                            duration: Duration::from_millis(20),
                            ..INSTANT
                        },
                    );
                }
            }
            Command::SetCueVolume { index, volume } => {
                let volume = volume.clamp(0.0, 2.0);
                if let Some(c) = self.plan.cues.get_mut(index) {
                    c.volume = volume;
                    for r in &self.running {
                        // Not while it is being stopped with a fade (as the web).
                        if let Running::Audio {
                            index: i,
                            gain,
                            stopped: false,
                            ..
                        } = r
                            && *i == index
                        {
                            gain.set_volume(volume);
                        }
                    }
                }
            }
            Command::SetOptions(o) => self.options = o,
            Command::Peaks { path, reply } => self.peaks(path, reply),
            Command::Audition { index, from } => self.start_audition(index, from),
            Command::StopAudition => self.stop_audition(),
            Command::SetOutputs { show, audition } => self.set_outputs(show, audition),
            Command::Devices(list) => self.set_devices(list),
            Command::SetAuditionVolume(v) => {
                self.audition_volume = v.clamp(0.0, 1.0);
                if let Some(t) = &mut self.aud.track {
                    t.set_volume(amplitude_to_db(self.audition_volume), INSTANT);
                }
                self.dirty = true;
            }
        }
        self.dirty = true;
    }

    fn load_show(&mut self, plan: ShowPlan) {
        self.stop_audition();
        self.stop_everything_now();
        self.set_paused(false);
        self.generation += 1;
        self.prep.set_generation(self.generation);
        self.assets.clear();
        self.by_path.clear();
        self.standby = plan.standby.min(plan.cues.len());
        self.attach_plan(plan);
    }

    /// The show was edited (FORMAT.md fields, order, cues added or removed). Unlike
    /// `LoadShow`, nothing stops: running cues carry on (matched by id), standby stays on the
    /// same cue (web keepStandby), a playing cue's volume changes live, and only new files
    /// and new edits are prepared.
    fn update_plan(&mut self, plan: ShowPlan) {
        let standby_id = self.plan.cues.get(self.standby).map(|c| c.id.clone());
        let old_standby = self.standby;
        let ids: Vec<String> = self.plan.cues.iter().map(|c| c.id.clone()).collect();
        let new_index: HashMap<&str, usize> = plan
            .cues
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id.as_str(), i))
            .collect();
        let remap = |i: usize| {
            ids.get(i)
                .and_then(|id| new_index.get(id.as_str()).copied())
        };

        // Running and pending cues follow their id; a deleted cue stops.
        let mut keep = Vec::with_capacity(self.running.len());
        let mut gone = Vec::new();
        for mut r in std::mem::take(&mut self.running) {
            match remap(r.index()) {
                Some(n) => {
                    match &mut r {
                        Running::Audio { index, .. } | Running::Timer { index, .. } => *index = n,
                    }
                    keep.push(r);
                }
                None => gone.push(r),
            }
        }
        for r in gone {
            self.retire(r);
        }
        // The audition keeps playing through edits (you trim while listening); it follows its
        // cue, and stops if the cue was deleted.
        match self.audition.as_ref().map(|a| remap(a.index)) {
            Some(Some(n)) => {
                if let Some(a) = self.audition.as_mut() {
                    a.index = n;
                }
            }
            Some(None) => self.stop_audition(),
            None => {}
        }
        self.running = keep;
        self.pending = std::mem::take(&mut self.pending)
            .into_iter()
            .filter_map(|p| remap(p.index).map(|index| PendingStart { index }))
            .collect();

        let len = plan.cues.len();
        self.standby = standby_id
            .and_then(|id| new_index.get(id.as_str()).copied())
            .unwrap_or(old_standby.min(len));

        // Live volume (as the web): a playing cue whose volume changed follows at once.
        for r in &self.running {
            if let Running::Audio {
                index,
                gain,
                stopped: false,
                ..
            } = r
            {
                gain.set_volume(plan.cues[*index].volume);
            }
        }
        self.attach_plan(plan);
    }

    /// Take `plan` as the cue list and make sure every file and edit it plays is prepared,
    /// standby first, then outward.
    fn attach_plan(&mut self, plan: ShowPlan) {
        let mut fresh = Vec::new();
        self.cue_asset = plan
            .cues
            .iter()
            .map(|c| {
                if c.kind != CueKind::Audio {
                    return None;
                }
                let path = c.audio.clone()?;
                if let Some(&a) = self.by_path.get(&path) {
                    return Some(a);
                }
                self.assets.push(Asset {
                    path: path.clone(),
                    state: PrepState::Pending,
                    raw: None,
                    edits: HashMap::new(),
                    in_flight: Vec::new(),
                    peaks_waiting: Vec::new(),
                    bytes: 0,
                });
                let a = self.assets.len() - 1;
                self.by_path.insert(path, a);
                fresh.push(a);
                Some(a)
            })
            .collect();
        self.standby = self.standby.min(plan.cues.len());
        self.plan = plan;

        let mut order: Vec<usize> = (0..self.plan.cues.len()).collect();
        let s = self.standby as i64;
        order.sort_by_key(|&i| {
            let d = i as i64 - s;
            if d >= 0 { 2 * d } else { -2 * d - 1 }
        });
        let mut seen = vec![false; self.assets.len()];
        for i in order {
            if let Some(a) = self.cue_asset[i]
                && !seen[a]
            {
                seen[a] = true;
                if fresh.contains(&a) {
                    let edits = self.missing_edits(a);
                    self.assets[a].in_flight = edits.clone();
                    self.prep.submit(Job {
                        generation: self.generation,
                        asset: a,
                        path: self.assets[a].path.clone(),
                        edits,
                        raw: None,
                    });
                } else {
                    self.ensure_edits(a);
                }
            }
        }
    }

    /// Edits of asset `a` the plan plays that are neither ready nor being made.
    fn missing_edits(&self, a: usize) -> Vec<EditKey> {
        let asset = &self.assets[a];
        let mut out: Vec<EditKey> = Vec::new();
        for i in 0..self.plan.cues.len() {
            if self.cue_asset[i] == Some(a) {
                let k = self.edit_key(i);
                if !asset.edits.contains_key(&k)
                    && !asset.in_flight.contains(&k)
                    && !out.contains(&k)
                {
                    out.push(k);
                }
            }
        }
        out
    }

    /// Render edits a decoded file is missing (an edit changed). No-op while it's decoding:
    /// `on_prep` calls this again when the decode lands.
    fn ensure_edits(&mut self, a: usize) {
        if self.assets[a].state != PrepState::Ready {
            return;
        }
        let edits = self.missing_edits(a);
        if edits.is_empty() {
            return;
        }
        let Some(raw) = self.assets[a].raw.clone() else {
            return;
        };
        self.assets[a].in_flight.extend(edits.iter().cloned());
        self.prep.submit(Job {
            generation: self.generation,
            asset: a,
            path: self.assets[a].path.clone(),
            edits,
            raw: Some(raw),
        });
    }

    fn go(&mut self) {
        if let Some(t) = self.last_go
            && self.options.go_lockout_ms > 0
            && t.elapsed() < Duration::from_millis(self.options.go_lockout_ms)
        {
            tracing::debug!(
                "GO ignored: within the {} ms lockout",
                self.options.go_lockout_ms
            );
            return;
        }
        if self.plan.cues.is_empty() {
            self.notice("There are no cues to play.", true);
            return;
        }
        if self.standby >= self.plan.cues.len() {
            self.notice(
                "End of the cue list. Use Back to top or click a cue.",
                false,
            );
            return;
        }
        self.last_go = Some(Instant::now());
        // An audition is on its own output and keeps playing (thorncue-spec#9).
        self.fire(self.standby);
    }

    /// Fire cue `i`; standby moves to the cue after it.
    fn fire(&mut self, i: usize) {
        let t = Instant::now();
        self.standby = i + 1;
        // GO on a cue that is already playing (or waiting) restarts it (FORMAT.md 2.5.6).
        self.stop_cue(i);
        self.pending.retain(|p| p.index != i);
        match self.plan.cues[i].kind {
            CueKind::Audio => self.start_audio(i),
            CueKind::Silence => {
                // Stops every running cue immediately (no fade), then counts `duration`.
                // Like Stop all on the web, it also un-pauses.
                self.stop_everything_now();
                self.set_paused(false);
                let d = self.plan.cues[i].duration;
                self.start_timer(i, d);
            }
            CueKind::Fadeout => {
                // Every running audio cue fades from its current level to 0 over `duration`,
                // then stops. Cues stopped this way don't continue.
                let d = self.plan.cues[i].duration;
                for r in &mut self.running {
                    if let Running::Audio { gain, stopped, .. } = r
                        && !*stopped
                    {
                        gain.stop_with_fade(d);
                        *stopped = true;
                    }
                }
                self.start_timer(i, d);
            }
        }
        self.dirty = true;
        tracing::debug!("GO on cue {} handled in {:?}", i + 1, t.elapsed());
    }

    fn start_timer(&mut self, i: usize, seconds: f64) {
        let now = self.clock_now().map(|t| t.ticks).unwrap_or(0);
        let end = now + (seconds * CLOCK_RATE).round() as u64;
        self.running.push(Running::Timer {
            index: i,
            start: now,
            end,
        });
    }

    fn start_audio(&mut self, i: usize) {
        let Some(a) = self.cue_asset[i] else {
            let q = self.q(i);
            self.notice(format!("{q} can't play: it has no audio file."), true);
            return;
        };
        match &self.assets[a].state {
            PrepState::Ready => {
                if self.assets[a].edits.contains_key(&self.edit_key(i)) {
                    self.play_now(i);
                } else {
                    // Its edit changed and is still rendering: start when it's ready (and,
                    // like a decoding cue, never after Stop all).
                    self.pending.push(PendingStart { index: i });
                    self.ensure_edits(a);
                }
            }
            PrepState::Pending | PrepState::Decoding => {
                self.pending.push(PendingStart { index: i })
            }
            PrepState::Missing => {
                let q = self.q(i);
                self.notice(
                    format!("{q} can't play: its audio file is missing. Re-import the show."),
                    true,
                );
            }
            PrepState::Failed(_) => {
                let q = self.q(i);
                self.notice(format!("{q} can't play: its audio file is damaged or in a format ThornCue can't read."), true);
            }
            PrepState::NotAudio => {}
        }
    }

    fn play_now(&mut self, i: usize) {
        let pre_wait = self.plan.cues[i].pre_wait;
        self.play_from(i, 0.0, pre_wait);
    }

    /// Start cue `i` `offset` seconds into its edited audio, after `pre_wait` seconds. GO uses
    /// 0 and the cue's pre-wait; moving to another show output uses where the cue was.
    fn play_from(&mut self, i: usize, offset: f64, pre_wait: f64) {
        let Some(a) = self.cue_asset[i] else { return };
        let key = self.edit_key(i);
        let Some(edited) = self.assets[a].edits.get(&key).cloned() else {
            tracing::error!("cue {i}: edit not prepared");
            return;
        };
        let cue = self.plan.cues[i].clone();
        let Some(mut data) = edited else {
            // Cuts remove everything: length 0. It ends at once (after its pre-wait) and
            // continues (FORMAT.md 2.5 "Edited audio").
            self.start_timer(i, pre_wait);
            return;
        };
        let (Some(show), Some(now)) = (
            self.show_track.as_mut(),
            self.show_clock.as_ref().map(|c| c.time()),
        ) else {
            let q = self.q(i);
            self.notice(
                format!("{q} can't play: there is no audio output. Press Restart audio engine."),
                true,
            );
            return;
        };
        let length = data.duration().as_secs_f64();
        // Sound and gain start at the same show-clock time, so the envelope lines up with the
        // first sample; the clock pauses with the show track, so Pause freezes pre-waits.
        let start = ClockTime {
            ticks: now.ticks + (pre_wait * CLOCK_RATE).round() as u64,
            ..now
        };
        data.settings.start_time = StartTime::ClockTime(start);
        if cue.looping {
            data = data.loop_region(..);
        }
        let offset = if cue.looping && length > 0.0 {
            offset % length
        } else {
            offset.min(length)
        };
        if offset > 0.0 {
            data.settings.start_position = kira::sound::PlaybackPosition::Seconds(offset);
        }
        let mut builder = TrackBuilder::new().sound_capacity(2);
        let gain = builder.add_effect(CueGainBuilder {
            volume: cue.volume,
            fade_in: cue.fade_in,
            fade_out: cue.fade_out,
            length,
            looping: cue.looping,
            start: Some(start),
            offset,
        });
        let mut track = match show.add_sub_track(builder) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("no free track for cue {i}: {e}");
                let q = self.q(i);
                self.notice(
                    format!("{q} can't play: too many cues are running at once."),
                    true,
                );
                return;
            }
        };
        let sound = match track.play(data) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("playing cue {i}: {e}");
                let q = self.q(i);
                self.notice(
                    format!("{q} couldn't start. Press Restart audio engine."),
                    true,
                );
                return;
            }
        };
        self.running.push(Running::Audio {
            index: i,
            track,
            sound,
            gain,
            length,
            looping: cue.looping,
            stopped: false,
            start: start.ticks,
            offset,
        });
    }

    /// Stop one cue at once (re-GO). It doesn't continue.
    fn stop_cue(&mut self, i: usize) {
        let mut k = 0;
        while k < self.running.len() {
            if self.running[k].index() == i {
                let r = self.running.remove(k);
                self.retire(r);
            } else {
                k += 1;
            }
        }
    }

    /// Take a running entry out of play at once.
    fn retire(&mut self, r: Running) {
        if let Running::Audio {
            track, mut sound, ..
        } = r
        {
            sound.stop(INSTANT);
            self.stopping.push((track, sound));
        }
    }

    /// Stop all always wins (CLAUDE.md rule 3): every running cue, every pending start, every
    /// silence/fade-out timer. With a stop fade set (and not a panic), audio fades out.
    ///
    /// It deliberately leaves a running audition alone: that is on a different output, and
    /// the operator may be previewing the next cue while stopping the show. The audition has
    /// its own Stop (thorncue-spec#9, ARCHITECTURE.md "Stop all and the audition").
    fn stop_all(&mut self, panic: bool) {
        let fade = if panic { 0.0 } else { self.options.stop_fade };
        if fade > 0.0 {
            self.pending.clear();
            for r in std::mem::take(&mut self.running) {
                match r {
                    Running::Audio {
                        index,
                        track,
                        sound,
                        gain,
                        length,
                        looping,
                        start,
                        offset,
                        ..
                    } => {
                        // A cue already fading (fade-out cue) keeps its own fade.
                        gain.stop_with_fade(fade);
                        self.running.push(Running::Audio {
                            index,
                            track,
                            sound,
                            gain,
                            length,
                            looping,
                            stopped: true,
                            start,
                            offset,
                        });
                    }
                    timer @ Running::Timer { .. } => self.retire(timer),
                }
            }
        } else {
            self.stop_everything_now();
        }
        // As the web: Stop all also un-pauses, so the next GO is heard.
        self.set_paused(false);
    }

    fn stop_everything_now(&mut self) {
        self.pending.clear();
        for r in std::mem::take(&mut self.running) {
            self.retire(r);
        }
    }

    fn set_paused(&mut self, paused: bool) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        // Pausing the show track stops it processing its child tracks (kira track/sub.rs
        // returns before processing sub-tracks), so every cue, fade and pre-wait freezes;
        // the show clock pauses with it, so silence and fade-out timers freeze too.
        // Both commands are read at the same block. Covered by tests.
        if let Some(t) = &mut self.show_track {
            if paused {
                t.pause(INSTANT)
            } else {
                t.resume(INSTANT)
            }
        }
        if let Some(c) = &mut self.show_clock {
            if paused { c.pause() } else { c.start() }
        }
    }

    fn peaks(&mut self, path: PathBuf, reply: crate::peaks::PeaksReply) {
        let Some(&a) = self.by_path.get(&path) else {
            let _ = reply.send(Err("That audio file isn't in the show.".into()));
            return;
        };
        let asset = &mut self.assets[a];
        match (&asset.state, &asset.raw) {
            (PrepState::Ready, Some(raw)) => {
                self.prep
                    .submit_peaks(raw.clone(), crate::peaks::PEAK_COLUMNS, reply)
            }
            (PrepState::Pending | PrepState::Decoding, _) => asset.peaks_waiting.push(reply),
            (PrepState::Missing, _) => {
                let _ = reply.send(Err("The audio file is missing.".into()));
            }
            _ => {
                let _ = reply.send(Err("The audio file can't be decoded.".into()));
            }
        }
    }

    fn start_audition(&mut self, i: usize, from: f64) {
        self.stop_audition();
        // Only ever on the audition output. Blocked: say why and do nothing else.
        let blocked = match self.audition_state() {
            AuditionState::Ready { .. } => None,
            AuditionState::Opening { name } => Some(format!(
                "The audition output \"{name}\" is still opening. Try again in a moment."
            )),
            AuditionState::Blocked { message, .. } => Some(message),
        };
        if let Some(message) = blocked {
            self.notice(message, true);
            return;
        }
        let Some(cue) = self.plan.cues.get(i).cloned() else {
            return;
        };
        let ready = self.cue_asset[i].and_then(|a| {
            let asset = &self.assets[a];
            let raw = asset.raw.clone()?;
            let edited = asset.edits.get(&self.edit_key(i))?.clone();
            Some((raw, edited))
        });
        let Some((raw, edited)) = ready else {
            let q = self.q(i);
            self.notice(
                format!("{q} isn't loaded yet, so it can't be auditioned."),
                true,
            );
            return;
        };
        let duration = raw.frames.len() as f64 / raw.sample_rate as f64;
        let from = from.clamp(0.0, duration);
        let out = cue.trim_end.map_or(duration, |e| e.min(duration));
        let segs = crate::edit::kept_segments(cue.trim_start, cue.trim_end, &cue.cuts, duration);
        // From inside the edit: hear the cue as it plays from that point, fades included (the
        // web's auditions ignored fades). Before the in point or after the out point: hear the
        // file itself from there, at the cue's volume (so you can find a new in or out point).
        let (data, segments, offset, envelope) = match edited {
            Some(d) if from >= cue.trim_start && from < out && !segs.is_empty() => {
                let offset = edited_time(&segs, from);
                let length = d.duration().as_secs_f64();
                (d, segs, offset, Some(length))
            }
            _ if from < duration - 0.001 => {
                (raw.slice(from..duration), vec![(from, duration)], 0.0, None)
            }
            _ => return,
        };
        let Some(out_track) = self.aud.track.as_mut() else {
            return;
        };
        let mut data = data;
        data.settings.start_position = kira::sound::PlaybackPosition::Seconds(offset);
        let mut builder = TrackBuilder::new().sound_capacity(1);
        let gain = builder.add_effect(match envelope {
            Some(length) => CueGainBuilder {
                volume: cue.volume,
                fade_in: cue.fade_in,
                fade_out: cue.fade_out,
                length,
                looping: cue.looping,
                start: None,
                offset,
            },
            None => CueGainBuilder {
                volume: cue.volume,
                fade_in: 0.0,
                fade_out: 0.0,
                length: f64::MAX,
                looping: false,
                start: None,
                offset: 0.0,
            },
        });
        let Ok(mut track) = out_track.add_sub_track(builder) else {
            return;
        };
        match track.play(data) {
            Ok(sound) => {
                self.audition = Some(Audition {
                    index: i,
                    track,
                    sound,
                    gain,
                    segments,
                    offset,
                });
                self.dirty = true;
            }
            Err(e) => tracing::error!("audition of cue {i}: {e}"),
        }
    }

    fn stop_audition(&mut self) {
        if let Some(mut a) = self.audition.take() {
            a.sound.stop(INSTANT);
            self.stopping.push((a.track, a.sound));
            self.dirty = true;
        }
    }

    // -----------------------------------------------------------------------
    // Outputs (thorncue-spec#9)
    // -----------------------------------------------------------------------

    fn show_route(&self) -> ShowRoute {
        show_route(&self.show_choice, &self.devices)
    }

    fn wanted_audition(&self) -> Result<AuditionRoute, AuditionBlock> {
        audition_route(&self.audition_choice, &self.show_route(), &self.devices)
    }

    pub fn audition_state(&self) -> AuditionState {
        match self.wanted_audition() {
            Err(reason) => AuditionState::Blocked {
                message: reason.message(),
                reason,
            },
            Ok(route) => {
                if let Some((r, reason)) = &self.aud.failed
                    && *r == route
                {
                    AuditionState::Blocked {
                        message: reason.message(),
                        reason: reason.clone(),
                    }
                } else if self.aud.manager.is_some() && self.aud.open_for.as_ref() == Some(&route) {
                    AuditionState::Ready {
                        name: route.name,
                        through_show: route.through_show,
                    }
                } else {
                    AuditionState::Opening { name: route.name }
                }
            }
        }
    }

    fn set_outputs(&mut self, show: ShowOutput, audition: AuditionOutput) {
        let choice_changed = audition != self.audition_choice || show != self.show_choice;
        self.show_choice = show;
        self.audition_choice = audition;
        if choice_changed {
            // A new choice deserves a fresh try.
            self.aud.failed = None;
        }
        let device = self.show_route().device;
        if device != self.show_device {
            self.move_show(device);
        }
        self.reconcile_audition();
        self.dirty = true;
    }

    fn set_devices(&mut self, list: DeviceList) {
        if list == self.devices {
            return;
        }
        self.devices = list;
        // The devices changed: a failed audition route may work now (headphones back in).
        self.aud.failed = None;
        // The show output isn't reopened here: if its device went, kira already moved it to
        // the system default and the bar warns; Restart audio engine (or choosing it again)
        // goes back to it.
        self.reconcile_audition();
        self.dirty = true;
    }

    /// Open, keep or close the audition output so it matches the choice and the devices.
    fn reconcile_audition(&mut self) {
        let wanted = self.wanted_audition();
        let keep = matches!(&wanted, Ok(r) if self.aud.open_for.as_ref() == Some(r));
        if keep {
            return;
        }
        // Whatever is open is wrong now (blocked, or another device): stop it at once.
        if self.audition.is_some() {
            if let Err(reason) = &wanted {
                self.notice(format!("Audition stopped. {}", reason.message()), true);
            }
            self.stop_audition();
        }
        self.close_audition_output();
        let Ok(route) = wanted else { return };
        if self.aud.failed.as_ref().is_some_and(|(r, _)| *r == route) {
            return;
        }
        let Some(opener) = self.aud.opener.as_mut() else {
            self.aud.failed = Some((
                route,
                AuditionBlock::Failed("there is no audition output in this build.".into()),
            ));
            return;
        };
        self.aud.opening = Some(opener(route.device.clone()));
        self.aud.open_for = Some(route);
        self.poll_audition_open();
    }

    fn close_audition_output(&mut self) {
        self.audition = None;
        self.aud.track = None;
        self.aud.manager = None;
        self.aud.opening = None;
        self.aud.open_for = None;
    }

    /// The opener's answer, when it has come.
    fn poll_audition_open(&mut self) {
        let Some(rx) = &self.aud.opening else { return };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(crossbeam_channel::TryRecvError::Empty) => return,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                Err("it didn't open.".to_string())
            }
        };
        self.aud.opening = None;
        let route = self.aud.open_for.clone().expect("opening has a route");
        let volume = amplitude_to_db(self.audition_volume);
        let built = result.and_then(|mut m| {
            m.add_sub_track(TrackBuilder::new().sub_track_capacity(4).volume(volume))
                .map(|t| (m, t))
                .map_err(|e| e.to_string())
        });
        match built {
            Ok((m, t)) => {
                tracing::info!("audition output open on {}", route.name);
                self.aud.manager = Some(m);
                self.aud.track = Some(t);
            }
            Err(e) => {
                tracing::warn!("audition output {}: {e}", route.name);
                self.aud.open_for = None;
                self.aud.failed = Some((route, AuditionBlock::Failed(e)));
            }
        }
        self.dirty = true;
    }

    /// The audition device reported it's gone: stop at once, never reroute (rule 2).
    fn check_audition_device(&mut self) {
        let lost = self
            .aud
            .manager
            .as_mut()
            .and_then(|m| m.backend_mut().take_device_error());
        let Some(err) = lost else { return };
        let route = self.aud.open_for.clone();
        tracing::warn!("audition output lost: {err}");
        let playing = self.audition.is_some();
        self.close_audition_output();
        if let Some(route) = route {
            let reason = AuditionBlock::Missing(route.name.clone());
            if playing {
                self.notice(format!("Audition stopped. {}", reason.message()), true);
            }
            self.aud.failed = Some((route, reason));
        }
        self.dirty = true;
    }

    /// Changing the show output during a show moves running cues: each carries on from where
    /// it was on the new device (after a short gap), with its fades where they were; pre-waits
    /// keep their remaining time; Pause stays as it was. Cues already stopping with a fade are
    /// let go. ARCHITECTURE.md "Output devices".
    fn move_show(&mut self, device: Option<OutputDevice>) {
        let now = self.clock_now().map(|t| t.ticks).unwrap_or(0);
        struct Carry {
            index: usize,
            offset: f64,
            pre_wait: f64,
        }
        let mut carry = Vec::new();
        let mut timers = Vec::new();
        for r in &self.running {
            match r {
                Running::Audio {
                    index,
                    gain,
                    stopped: false,
                    start,
                    offset,
                    ..
                } => carry.push(Carry {
                    index: *index,
                    offset: offset + gain.elapsed(),
                    pre_wait: start.saturating_sub(now) as f64 / CLOCK_RATE,
                }),
                Running::Audio { .. } => {}
                Running::Timer { index, start, end } => {
                    timers.push((*index, now.saturating_sub(*start), end.saturating_sub(now)))
                }
            }
        }
        let paused = self.paused;
        let pending = std::mem::take(&mut self.pending);
        self.running.clear();
        self.stopping.clear();
        self.show_track = None;
        self.show_clock = None;
        self.heartbeat = None;
        self.paused = false;
        self.manager = None;
        self.show_device = device.clone();
        match (self.factory)(device.as_ref()) {
            Ok(m) => {
                self.manager = Some(m);
                self.status = EngineStatus::Ok;
                self.status_message = None;
                self.build_mix();
            }
            Err(e) => {
                tracing::error!("opening the show output: {e}");
                self.status = EngineStatus::NoOutput;
                self.status_message = Some(no_output_message());
                self.notice(no_output_message(), true);
                return;
            }
        }
        let now = self.clock_now().map(|t| t.ticks).unwrap_or(0);
        for (index, done, left) in timers {
            self.running.push(Running::Timer {
                index,
                start: now.saturating_sub(done),
                end: now + left,
            });
        }
        for c in carry {
            self.play_from(c.index, c.offset, c.pre_wait);
        }
        self.pending = pending;
        if paused {
            self.set_paused(true);
        }
        let name = self.show_route().name;
        self.notice(format!("The show now plays on {name}."), false);
        self.dirty = true;
    }

    fn toggle_pause(&mut self) {
        if self.show_track.is_some() {
            self.set_paused(!self.paused);
        }
    }

    /// Restart audio engine: the show output only. A running audition is on its own output
    /// and is left alone (thorncue-spec#9).
    fn restart(&mut self) {
        self.stop_everything_now();
        self.stopping.clear();
        self.show_track = None;
        self.show_clock = None;
        self.heartbeat = None;
        self.paused = false;
        self.manager = None;
        let t = Instant::now();
        self.show_device = self.show_route().device;
        match (self.factory)(self.show_device.as_ref()) {
            Ok(m) => {
                self.manager = Some(m);
                self.status = EngineStatus::Ok;
                self.status_message = None;
                self.build_mix();
                tracing::info!("audio engine restarted in {:?}", t.elapsed());
                self.notice("Audio engine restarted.", false);
            }
            Err(e) => {
                tracing::error!("restarting audio: {e}");
                self.status = EngineStatus::NoOutput;
                self.status_message = Some(no_output_message());
                self.notice(no_output_message(), true);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Prep results and ticks
    // -----------------------------------------------------------------------

    pub(crate) fn on_prep(&mut self, msg: PrepMsg) {
        match msg {
            PrepMsg::Started { generation, asset } => {
                if generation == self.generation
                    && matches!(self.assets[asset].state, PrepState::Pending)
                {
                    self.assets[asset].state = PrepState::Decoding;
                    self.dirty = true;
                }
            }
            PrepMsg::Finished {
                generation,
                asset,
                result,
            } => {
                if generation != self.generation {
                    return;
                }
                self.dirty = true;
                match *result {
                    Ok(decoded) => {
                        let a = &mut self.assets[asset];
                        a.state = PrepState::Ready;
                        if a.raw.is_none() {
                            a.raw = Some(decoded.raw);
                        }
                        for (k, d) in decoded.edits {
                            a.in_flight.retain(|x| x != &k);
                            a.edits.insert(k, d);
                        }
                        let frame = std::mem::size_of::<kira::Frame>() as u64;
                        let raw_frames = a.raw.as_ref().map(|r| r.frames.clone());
                        let mut bytes = raw_frames.as_ref().map_or(0, |f| f.len() as u64 * frame);
                        for d in a.edits.values().flatten() {
                            if !raw_frames
                                .as_ref()
                                .is_some_and(|f| std::sync::Arc::ptr_eq(&d.frames, f))
                            {
                                bytes += d.frames.len() as u64 * frame;
                            }
                        }
                        a.bytes = bytes;
                        for reply in std::mem::take(&mut a.peaks_waiting) {
                            if let Some(raw) = a.raw.clone() {
                                self.prep
                                    .submit_peaks(raw, crate::peaks::PEAK_COLUMNS, reply);
                            }
                        }
                        // Edits added while it was decoding.
                        self.ensure_edits(asset);
                    }
                    Err(e) if e == MISSING => {
                        self.assets[asset].state = PrepState::Missing;
                        for reply in std::mem::take(&mut self.assets[asset].peaks_waiting) {
                            let _ = reply.send(Err("The audio file is missing.".into()));
                        }
                    }
                    Err(e) => {
                        tracing::warn!("can't decode {}: {e}", self.assets[asset].path.display());
                        self.assets[asset].state = PrepState::Failed(e);
                        for reply in std::mem::take(&mut self.assets[asset].peaks_waiting) {
                            let _ = reply.send(Err("The audio file can't be decoded.".into()));
                        }
                    }
                }
                // Start any GO that was waiting for this file (and wasn't cancelled by Stop all).
                let waiting: Vec<usize> = self
                    .pending
                    .iter()
                    .filter(|p| self.cue_asset[p.index] == Some(asset))
                    .map(|p| p.index)
                    .collect();
                self.pending
                    .retain(|p| self.cue_asset[p.index] != Some(asset));
                for i in waiting {
                    self.start_audio(i);
                }
            }
        }
    }

    /// Drain finished prep work without blocking. Returns how many messages were handled.
    pub fn poll_prep(&mut self) -> usize {
        let rx = self.prep.results.clone();
        let mut n = 0;
        while let Ok(msg) = rx.try_recv() {
            self.on_prep(msg);
            n += 1;
        }
        n
    }

    /// Bookkeeping: retire finished cues and fire their continues, watch the device, and
    /// publish state and positions.
    pub fn tick(&mut self) {
        // Device trouble: an error the backend recognises, or the audio clock standing still.
        if let Some(err) = self.backend_mut().and_then(|b| b.take_device_error()) {
            tracing::error!("audio device problem: {err}");
            // A reported disconnect beats a stall alarm and stays until Restart: kira may have
            // moved the output to the default device (the computer's own speakers).
            if self.lost_by_stall {
                self.status = EngineStatus::Ok;
                self.lost_by_stall = false;
            }
            self.device_lost(
                "The audio output was disconnected. Sound may now be coming from the computer's own speakers. Reconnect the interface and press Restart audio engine.",
            );
        }
        self.check_heartbeat();
        self.poll_audition_open();
        self.check_audition_device();
        self.retire_finished();
        if self
            .audition
            .as_ref()
            .is_some_and(|a| a.sound.state() == PlaybackState::Stopped)
        {
            self.stop_audition();
            self.dirty = true;
        }
        self.stopping
            .retain(|(_, s)| s.state() != PlaybackState::Stopped);

        let phases: Vec<(usize, RunPhase)> = self
            .running
            .iter()
            .map(|r| (r.index(), self.phase_of(r)))
            .collect();
        if phases != self.last_phases {
            self.last_phases = phases;
            self.dirty = true;
        }
        if self.dirty {
            self.dirty = false;
            self.revision += 1;
            let snap = self.snapshot();
            (self.out)(Event::State(snap));
        }
        if (!self.running.is_empty() || self.audition.is_some())
            && self.last_positions.elapsed() >= Duration::from_millis(33)
        {
            self.last_positions = Instant::now();
            let p = self.positions();
            (self.out)(Event::Positions(p));
        }
    }

    /// Cues that ended: remove them, and fire `continue: after_end` for those that ended by
    /// themselves (never for stopped or looping cues).
    fn retire_finished(&mut self) {
        let now = self.clock_now().map(|t| t.ticks);
        let mut continues = Vec::new();
        let mut k = 0;
        while k < self.running.len() {
            let (done, natural) = match &self.running[k] {
                Running::Audio {
                    sound,
                    gain,
                    stopped,
                    looping,
                    ..
                } => {
                    if gain.phase() == GainPhase::Silent {
                        (true, false)
                    } else if sound.state() == PlaybackState::Stopped {
                        (true, !*stopped && !*looping)
                    } else {
                        (false, false)
                    }
                }
                Running::Timer { end, .. } => (now.is_some_and(|n| n >= *end), true),
            };
            if done {
                let r = self.running.remove(k);
                let i = r.index();
                self.retire(r);
                self.dirty = true;
                if natural && self.plan.cues.get(i).is_some_and(|c| c.continue_after) {
                    continues.push(i);
                }
            } else {
                k += 1;
            }
        }
        for i in continues {
            if i + 1 < self.plan.cues.len() {
                tracing::debug!("cue {} ended; continuing to {}", i + 1, i + 2);
                self.fire(i + 1);
            }
        }
    }

    fn device_lost(&mut self, message: &str) {
        if self.status != EngineStatus::DeviceLost {
            self.status = EngineStatus::DeviceLost;
            self.status_message = Some(message.into());
            self.notice(message.to_string(), true);
            self.dirty = true;
        }
    }

    /// The device pulls audio continuously, so the heartbeat clock should always move. If it
    /// stands still for `STALL`, nothing is reaching the speakers, whatever the cue list says.
    /// Thresholds are generous: PulseAudio/PipeWire can hiccup for a few hundred ms under load,
    /// and a false alarm mid-show (inviting a Restart that stops every cue) is worse than a
    /// couple of seconds' delay in reporting a dead output.
    fn check_heartbeat(&mut self) {
        let watch = self
            .manager
            .as_mut()
            .is_some_and(|m| m.backend_mut().watch_for_stalls());
        let Some(clock) = &self.heartbeat else { return };
        if !watch {
            return;
        }
        let watching_for_recovery = self.status == EngineStatus::DeviceLost && self.lost_by_stall;
        if self.status != EngineStatus::Ok && !watching_for_recovery {
            return;
        }
        let ticks = clock.time().ticks;
        if ticks != self.last_beat.0 {
            let gap = self.last_beat.1.elapsed();
            if gap >= Duration::from_millis(100) && self.last_beat.2 {
                tracing::debug!("audio clock gap: {gap:?}");
            }
            self.last_beat = (ticks, Instant::now(), true);
            if watching_for_recovery {
                let since = *self.recovering_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= RECOVERED {
                    tracing::info!("the audio output is pulling audio again");
                    self.status = EngineStatus::Ok;
                    self.status_message = None;
                    self.lost_by_stall = false;
                    self.recovering_since = None;
                    self.notice("The audio output is working again.", false);
                    self.dirty = true;
                }
            }
            return;
        }
        // A freshly opened output can take a moment to deliver its first callback
        // (PulseAudio reconnecting, Bluetooth waking up): allow longer until it first moves.
        let limit = if self.last_beat.2 { STALL } else { FIRST_BEAT };
        if self.last_beat.1.elapsed() < limit {
            return;
        }
        self.recovering_since = None;
        if !watching_for_recovery {
            tracing::error!(
                "the audio output has stopped pulling audio for {:?}",
                self.last_beat.1.elapsed()
            );
            self.device_lost(
                "The audio output has stopped. Check the interface is connected and switched on, then press Restart audio engine.",
            );
            self.lost_by_stall = true;
        }
    }

    pub fn positions(&self) -> Vec<Position> {
        let now = self.clock_now().map(|t| t.ticks).unwrap_or(0);
        self.running
            .iter()
            .map(|r| match r {
                Running::Audio {
                    index,
                    gain,
                    length,
                    looping,
                    offset,
                    ..
                } => {
                    let elapsed = offset + gain.elapsed();
                    Position {
                        index: *index,
                        id: self.plan.cues[*index].id.clone(),
                        elapsed,
                        remaining: (!looping).then(|| (length - elapsed).max(0.0)),
                        length: *length,
                        control: false,
                        audition: false,
                        source_time: None,
                    }
                }
                Running::Timer { index, start, end } => {
                    let length = (end - start) as f64 / CLOCK_RATE;
                    let elapsed = (now.saturating_sub(*start) as f64 / CLOCK_RATE).min(length);
                    Position {
                        index: *index,
                        id: self.plan.cues[*index].id.clone(),
                        elapsed,
                        remaining: Some(length - elapsed),
                        length,
                        control: self.plan.cues[*index].kind != CueKind::Audio,
                        audition: false,
                        source_time: None,
                    }
                }
            })
            .chain(self.audition.as_ref().map(|a| {
                let total: f64 = a.segments.iter().map(|(s, e)| e - s).sum();
                let elapsed = a.offset + a.gain.elapsed();
                Position {
                    index: a.index,
                    id: self
                        .plan
                        .cues
                        .get(a.index)
                        .map(|c| c.id.clone())
                        .unwrap_or_default(),
                    elapsed,
                    remaining: Some((total - elapsed).max(0.0)),
                    length: total,
                    control: false,
                    audition: true,
                    source_time: Some(a.source_time()),
                }
            }))
            .collect()
    }

    fn phase_of(&self, r: &Running) -> RunPhase {
        match r {
            // A cue we've stopped with a fade shows as fading at once, before the audio
            // thread has picked the fade up.
            Running::Audio { stopped: true, .. } => RunPhase::Fading,
            Running::Audio { gain, .. } => match gain.phase() {
                GainPhase::Waiting => RunPhase::Waiting,
                GainPhase::Playing => RunPhase::Running,
                GainPhase::Fading | GainPhase::Silent => RunPhase::Fading,
            },
            Running::Timer { .. } => RunPhase::Running,
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut running: Vec<RunningCue> = self
            .pending
            .iter()
            .map(|p| RunningCue {
                index: p.index,
                id: self.plan.cues[p.index].id.clone(),
                phase: RunPhase::Starting,
            })
            .collect();
        running.extend(self.running.iter().map(|r| {
            let index = r.index();
            RunningCue {
                index,
                id: self.plan.cues[index].id.clone(),
                phase: self.phase_of(r),
            }
        }));
        let prep = (0..self.plan.cues.len())
            .map(|i| {
                let c = &self.plan.cues[i];
                match self.cue_asset[i] {
                    Some(a) => {
                        let asset = &self.assets[a];
                        let length = match asset.edits.get(&self.edit_key(i)) {
                            Some(Some(d)) => Some(d.duration().as_secs_f64()),
                            Some(None) => Some(0.0),
                            None => None,
                        };
                        CuePrep {
                            state: asset.state.clone(),
                            length,
                        }
                    }
                    None if c.kind == CueKind::Audio => CuePrep {
                        state: PrepState::Missing,
                        length: None,
                    },
                    None => CuePrep {
                        state: PrepState::NotAudio,
                        length: Some(c.duration),
                    },
                }
            })
            .collect();
        Snapshot {
            revision: self.revision,
            status: self.status,
            status_message: self.status_message.clone(),
            paused: self.paused,
            standby: self.standby,
            cue_count: self.plan.cues.len(),
            running,
            prep,
            prep_done: self.assets.iter().filter(|a| a.state.is_settled()).count(),
            prep_total: self.assets.len(),
            audition: self.audition.as_ref().map(|a| a.index),
            decoded_bytes: self.assets.iter().map(|a| a.bytes).sum(),
            master: self.master,
            outputs: OutputsState {
                show_choice: self.show_choice.clone(),
                audition_choice: self.audition_choice.clone(),
                show: self.show_route(),
                audition: self.audition_state(),
                audition_volume: self.audition_volume,
                devices: self.devices.devices.clone(),
                default_device: self.devices.default.clone(),
            },
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn standby(&self) -> usize {
        self.standby
    }

    /// Cue indexes running or waiting to start.
    pub fn running_indexes(&self) -> Vec<usize> {
        self.snapshot().running.iter().map(|r| r.index).collect()
    }

    /// Seconds cue `i` has played (output time since its first sample).
    pub fn position_of(&self, i: usize) -> Option<f64> {
        self.positions()
            .into_iter()
            .find(|p| p.index == i)
            .map(|p| p.elapsed)
    }
}

/// How long the audio clock may stand still before the output counts as dead.
const STALL: Duration = Duration::from_secs(2);
/// How long a just-opened output may take to start pulling audio.
const FIRST_BEAT: Duration = Duration::from_secs(5);
/// How long audio must flow steadily before a stall alarm clears itself.
const RECOVERED: Duration = Duration::from_secs(1);

fn no_output_message() -> String {
    "ThornCue couldn't open an audio output. Check an output device is connected and press Restart audio engine.".into()
}
