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
use crate::plan::{CueKind, ShowPlan};
use crate::prep::{Job, Loader, MISSING, PrepMsg, PrepPool};
use crate::state::{
    CuePrep, EngineStatus, Event, Notice, Position, PrepState, RunPhase, RunningCue, Snapshot,
};

/// Builds the audio manager. Called on the engine thread at start and on Restart audio engine.
pub type ManagerFactory<B> = Box<dyn FnMut() -> Result<AudioManager<B>, String> + Send>;

/// Everything the UI can ask for. Commands are intents; the engine decides what they mean.
#[derive(Debug, Clone, PartialEq)]
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

/// A GO on a cue whose audio is still decoding (W1).
struct PendingStart {
    index: usize,
}

pub struct Core<B: EngineBackend> {
    factory: ManagerFactory<B>,
    manager: Option<AudioManager<B>>,
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

    out: Box<dyn FnMut(Event) + Send>,
    revision: u64,
    dirty: bool,
    last_positions: Instant,
}

impl<B: EngineBackend> Core<B> {
    pub fn new(
        mut factory: ManagerFactory<B>,
        loader: Loader,
        out: Box<dyn FnMut(Event) + Send>,
    ) -> Self {
        let (manager, status, status_message) = match factory() {
            Ok(m) => (Some(m), EngineStatus::Ok, None),
            Err(e) => {
                tracing::error!("couldn't open audio output: {e}");
                (None, EngineStatus::NoOutput, Some(no_output_message()))
            }
        };
        let mut core = Self {
            factory,
            manager,
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
            out,
            revision: 0,
            dirty: true,
            last_positions: Instant::now(),
        };
        core.build_mix();
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
        format!("Q{} \"{name}\"", index + 1)
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
                            "There is no Q{}. This show has {n} cue{}.",
                            i + 1,
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
        }
        self.dirty = true;
    }

    fn load_show(&mut self, plan: ShowPlan) {
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
            self.start_timer(i, cue.pre_wait);
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
            ticks: now.ticks + (cue.pre_wait * CLOCK_RATE).round() as u64,
            ..now
        };
        data.settings.start_time = StartTime::ClockTime(start);
        if cue.looping {
            data = data.loop_region(..);
        }
        let mut builder = TrackBuilder::new().sound_capacity(2);
        let gain = builder.add_effect(CueGainBuilder {
            volume: cue.volume,
            fade_in: cue.fade_in,
            fade_out: cue.fade_out,
            length,
            looping: cue.looping,
            start: Some(start),
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

    fn toggle_pause(&mut self) {
        if self.show_track.is_some() {
            self.set_paused(!self.paused);
        }
    }

    fn restart(&mut self) {
        self.stop_everything_now();
        self.stopping.clear();
        self.show_track = None;
        self.show_clock = None;
        self.heartbeat = None;
        self.paused = false;
        self.manager = None;
        let t = Instant::now();
        match (self.factory)() {
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
                        // Edits added while it was decoding.
                        self.ensure_edits(asset);
                    }
                    Err(e) if e == MISSING => self.assets[asset].state = PrepState::Missing,
                    Err(e) => {
                        tracing::warn!("can't decode {}: {e}", self.assets[asset].path.display());
                        self.assets[asset].state = PrepState::Failed(e);
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
        self.retire_finished();
        self.stopping
            .retain(|(_, s)| s.state() != PlaybackState::Stopped);

        if self.dirty {
            self.dirty = false;
            self.revision += 1;
            let snap = self.snapshot();
            (self.out)(Event::State(snap));
        }
        if !self.running.is_empty() && self.last_positions.elapsed() >= Duration::from_millis(33) {
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
                    ..
                } => {
                    let elapsed = gain.elapsed();
                    Position {
                        index: *index,
                        id: self.plan.cues[*index].id.clone(),
                        elapsed,
                        remaining: (!looping).then(|| (length - elapsed).max(0.0)),
                        length: *length,
                        control: false,
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
                    }
                }
            })
            .collect()
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
            let phase = match r {
                // A cue we've stopped with a fade shows as fading at once, before the audio
                // thread has picked the fade up.
                Running::Audio { stopped: true, .. } => RunPhase::Fading,
                Running::Audio { gain, .. } => match gain.phase() {
                    GainPhase::Waiting => RunPhase::Waiting,
                    GainPhase::Playing => RunPhase::Running,
                    GainPhase::Fading | GainPhase::Silent => RunPhase::Fading,
                },
                Running::Timer { .. } => RunPhase::Running,
            };
            let index = r.index();
            RunningCue {
                index,
                id: self.plan.cues[index].id.clone(),
                phase,
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
            decoded_bytes: self.assets.iter().map(|a| a.bytes).sum(),
            master: self.master,
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
