//! The engine proper. `Core` is synchronous: the engine thread (`thread.rs`) feeds it commands,
//! prep results and ticks; tests drive it directly with a `TestBackend`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use kira::clock::{ClockHandle, ClockSpeed};
use kira::sound::PlaybackState;
use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle};
use kira::track::{TrackBuilder, TrackHandle};
use kira::{AudioManager, Decibels, StartTime, Tween};

use crate::backend::EngineBackend;
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
    LoadShow(ShowPlan),
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
}

const INSTANT: Tween = Tween {
    start_time: StartTime::Immediate,
    duration: Duration::ZERO,
    easing: kira::Easing::Linear,
};

fn tween(seconds: f64) -> Tween {
    Tween {
        duration: Duration::from_secs_f64(seconds.max(0.0)),
        ..INSTANT
    }
}

/// Linear amplitude (FORMAT.md volume) to kira decibels. kira treats -60 dB as silence.
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
    data: Option<StaticSoundData>,
}

struct Running {
    index: usize,
    id: String,
    /// Each running cue has its own sub-track under the show track. Kept so the track
    /// lives as long as the sound; dropping it removes the track.
    _track: TrackHandle,
    sound: StaticSoundHandle,
    length: f64,
    pre_wait: f64,
    fade_out: f64,
    /// TODO(M4): CueGain. M3 starts the fade-out with a kira tween from the tick.
    fading_out: bool,
}

/// A GO on a cue whose audio is still decoding (W1).
struct PendingStart {
    index: usize,
}

pub struct Core<B: EngineBackend> {
    factory: ManagerFactory<B>,
    manager: Option<AudioManager<B>>,
    show_track: Option<TrackHandle>,
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
    generation: u64,
    prep: PrepPool,

    standby: usize,
    paused: bool,
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
            heartbeat: None,
            last_beat: (0, Instant::now(), false),
            lost_by_stall: false,
            recovering_since: None,
            status,
            status_message,
            plan: ShowPlan::default(),
            cue_asset: Vec::new(),
            assets: Vec::new(),
            generation: 0,
            prep: PrepPool::new(2, loader),
            standby: 0,
            paused: false,
            running: Vec::new(),
            pending: Vec::new(),
            stopping: Vec::new(),
            out,
            revision: 0,
            dirty: true,
            last_positions: Instant::now(),
        };
        core.make_show_track();
        core
    }

    fn make_show_track(&mut self) {
        self.show_track = None;
        self.heartbeat = None;
        self.last_beat = (0, Instant::now(), false);
        self.lost_by_stall = false;
        self.recovering_since = None;
        if let Some(m) = &mut self.manager {
            match m.add_clock(ClockSpeed::TicksPerSecond(1000.0)) {
                Ok(mut c) => {
                    c.start();
                    self.heartbeat = Some(c);
                }
                Err(e) => tracing::warn!("no heartbeat clock: {e}"),
            }
            match m.add_sub_track(TrackBuilder::new().sub_track_capacity(64)) {
                Ok(t) => self.show_track = Some(t),
                Err(e) => {
                    tracing::error!("couldn't create the show track: {e}");
                    self.status = EngineStatus::NoOutput;
                    self.status_message = Some(no_output_message());
                }
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

    // -----------------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------------

    pub fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::LoadShow(plan) => self.load_show(plan),
            Command::Go => self.go(),
            Command::StopAll { panic } => self.stop_all(panic),
            Command::TogglePause => self.toggle_pause(),
            Command::SetStandby(i) => self.set_standby(i),
            Command::MoveStandby(d) => {
                if !self.plan.cues.is_empty() {
                    let last = self.plan.cues.len() as i64 - 1;
                    let to = (self.standby as i64 + d as i64).clamp(0, last);
                    self.set_standby(to as usize);
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
        }
        self.dirty = true;
    }

    fn load_show(&mut self, plan: ShowPlan) {
        self.stop_everything_now();
        self.paused = false;
        if let Some(t) = &mut self.show_track {
            t.resume(INSTANT);
        }
        self.generation += 1;
        self.prep.set_generation(self.generation);

        let mut by_path: HashMap<PathBuf, usize> = HashMap::new();
        self.assets.clear();
        self.cue_asset = plan
            .cues
            .iter()
            .map(|c| {
                if c.kind != CueKind::Audio {
                    return None;
                }
                let path = c.audio.clone()?;
                Some(*by_path.entry(path.clone()).or_insert_with(|| {
                    self.assets.push(Asset {
                        path,
                        state: PrepState::Pending,
                        data: None,
                    });
                    self.assets.len() - 1
                }))
            })
            .collect();
        self.standby = plan.standby.min(plan.cues.len());
        self.plan = plan;

        // Standby cue first, then outward in show order.
        let mut order: Vec<usize> = (0..self.plan.cues.len()).collect();
        let s = self.standby as i64;
        order.sort_by_key(|&i| {
            let d = i as i64 - s;
            if d >= 0 { 2 * d } else { -2 * d - 1 }
        });
        let mut queued = vec![false; self.assets.len()];
        for i in order {
            if let Some(a) = self.cue_asset[i]
                && !queued[a]
            {
                queued[a] = true;
                self.prep.submit(Job {
                    generation: self.generation,
                    asset: a,
                    path: self.assets[a].path.clone(),
                });
            }
        }
    }

    fn go(&mut self) {
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
        self.fire(self.standby);
    }

    /// Fire cue `i`; standby moves to the cue after it.
    fn fire(&mut self, i: usize) {
        let t = Instant::now();
        self.standby = i + 1;
        let cue = &self.plan.cues[i];
        match cue.kind {
            CueKind::Audio => self.start_audio(i),
            // TODO(M4): silence and fade-out cues on the engine clock.
            CueKind::Silence | CueKind::Fadeout => {
                tracing::debug!("cue {i} is a {:?} cue; not played until M4", cue.kind);
            }
        }
        tracing::debug!("GO on cue {} handled in {:?}", i + 1, t.elapsed());
    }

    fn start_audio(&mut self, i: usize) {
        let Some(a) = self.cue_asset[i] else {
            let q = self.q(i);
            self.notice(format!("{q} can't play: it has no audio file."), true);
            return;
        };
        // GO on a cue that is already playing (or waiting) restarts it (FORMAT.md 2.5.6).
        self.stop_cue(i);
        self.pending.retain(|p| p.index != i);
        match &self.assets[a].state {
            PrepState::Ready => self.play_now(i),
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
        let Some(data) = self.assets[a].data.clone() else {
            return;
        };
        let Some(show) = self.show_track.as_mut() else {
            let q = self.q(i);
            self.notice(
                format!("{q} can't play: there is no audio output. Press Restart audio engine."),
                true,
            );
            return;
        };
        let cue = &self.plan.cues[i];
        let length = data.duration().as_secs_f64();
        let mut data = data.volume(amplitude_to_db(cue.volume));
        if cue.fade_in > 0.0 {
            // TODO(M4): CueGain (kira tweens are linear in dB, FORMAT.md wants amplitude).
            data = data.fade_in_tween(tween(cue.fade_in));
        }
        if cue.pre_wait > 0.0 {
            // Counts down only while the show track is advancing, so Pause freezes it.
            // Quantised to kira's 128-frame blocks (agreed tolerance).
            data.settings.start_time = StartTime::Delayed(Duration::from_secs_f64(cue.pre_wait));
        }
        let mut track = match show.add_sub_track(TrackBuilder::new().sound_capacity(2)) {
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
        self.running.push(Running {
            index: i,
            id: cue.id.clone(),
            _track: track,
            sound,
            length,
            pre_wait: cue.pre_wait,
            fade_out: if cue.looping { 0.0 } else { cue.fade_out },
            fading_out: false,
        });
    }

    fn stop_cue(&mut self, i: usize) {
        let mut k = 0;
        while k < self.running.len() {
            if self.running[k].index == i {
                let mut r = self.running.remove(k);
                r.sound.stop(INSTANT);
                self.stopping.push((r._track, r.sound));
            } else {
                k += 1;
            }
        }
    }

    /// Stop all always wins (CLAUDE.md rule 3): every running cue, every pending start.
    fn stop_all(&mut self, _panic: bool) {
        // TODO(M4): Stop-all fade setting; `panic` will skip it.
        self.stop_everything_now();
        // As the web: Stop all also un-pauses, so the next GO is heard.
        if self.paused {
            self.paused = false;
            if let Some(t) = &mut self.show_track {
                t.resume(INSTANT);
            }
        }
    }

    fn stop_everything_now(&mut self) {
        self.pending.clear();
        for mut r in self.running.drain(..) {
            r.sound.stop(INSTANT);
            self.stopping.push((r._track, r.sound));
        }
    }

    fn toggle_pause(&mut self) {
        let Some(t) = &mut self.show_track else {
            return;
        };
        // Pausing the parent track stops it processing its child tracks, so every cue,
        // fade and pre-wait freezes together (kira track/sub.rs: a paused track returns
        // before processing sub-tracks). Covered by a test.
        if self.paused {
            t.resume(INSTANT);
        } else {
            t.pause(INSTANT);
        }
        self.paused = !self.paused;
    }

    fn set_standby(&mut self, i: usize) {
        self.standby = i.min(self.plan.cues.len());
    }

    fn restart(&mut self) {
        self.stop_everything_now();
        self.stopping.clear();
        self.show_track = None;
        self.paused = false;
        self.manager = None;
        let t = Instant::now();
        match (self.factory)() {
            Ok(m) => {
                self.manager = Some(m);
                self.status = EngineStatus::Ok;
                self.status_message = None;
                self.make_show_track();
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
                    Ok(data) => {
                        self.assets[asset].state = PrepState::Ready;
                        self.assets[asset].data = Some(data);
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

    /// Bookkeeping: retire finished sounds, start scheduled fade-outs, watch the device,
    /// and publish state and positions.
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

        // Finished sounds.
        let before = self.running.len();
        self.running
            .retain(|r| r.sound.state() != PlaybackState::Stopped);
        if self.running.len() != before {
            // TODO(M4): continue: after_end fires here for cues that ended by themselves.
            self.dirty = true;
        }
        self.stopping
            .retain(|(_, s)| s.state() != PlaybackState::Stopped);

        // Scheduled fade-outs (M3: kira tween from here; M4 moves this into CueGain).
        if !self.paused {
            for r in &mut self.running {
                if r.fade_out > 0.0 && !r.fading_out {
                    let pos = r.sound.position();
                    if pos >= (r.length - r.fade_out).max(0.0) && pos > 0.0 {
                        r.fading_out = true;
                        r.sound.stop(tween((r.length - pos).max(0.0)));
                    }
                }
            }
        }

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
        self.running
            .iter()
            .map(|r| {
                let elapsed = r.sound.position();
                let looping = self.plan.cues.get(r.index).is_some_and(|c| c.looping);
                Position {
                    index: r.index,
                    id: r.id.clone(),
                    elapsed,
                    remaining: (!looping).then(|| (r.length - elapsed).max(0.0)),
                    length: r.length,
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
            let waiting = r.pre_wait > 0.0
                && r.sound.position() == 0.0
                && r.sound.state() == PlaybackState::Playing;
            RunningCue {
                index: r.index,
                id: r.id.clone(),
                phase: if r.fading_out && r.sound.state() == PlaybackState::Stopping {
                    RunPhase::Fading
                } else if waiting {
                    RunPhase::Waiting
                } else {
                    RunPhase::Running
                },
            }
        }));
        let prep = self
            .plan
            .cues
            .iter()
            .enumerate()
            .map(|(i, c)| match self.cue_asset[i] {
                Some(a) => CuePrep {
                    state: self.assets[a].state.clone(),
                    length: self.assets[a]
                        .data
                        .as_ref()
                        .map(|d| d.duration().as_secs_f64()),
                },
                None if c.kind == CueKind::Audio => CuePrep {
                    state: PrepState::Missing,
                    length: None,
                },
                None => CuePrep {
                    state: PrepState::NotAudio,
                    length: Some(c.duration),
                },
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
            decoded_bytes: self
                .assets
                .iter()
                .filter_map(|a| a.data.as_ref())
                .map(|d| (d.frames.len() * std::mem::size_of::<kira::Frame>()) as u64)
                .sum(),
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

    /// Position of the running cue `i`, in seconds.
    pub fn position_of(&self, i: usize) -> Option<f64> {
        self.running
            .iter()
            .find(|r| r.index == i)
            .map(|r| r.sound.position())
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
