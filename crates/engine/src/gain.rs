//! `CueGain`: the per-cue gain effect (docs/ARCHITECTURE.md "Playing a cue").
//!
//! kira's own volume tweens interpolate in decibels; FORMAT.md 2.5 wants fades linear in
//! amplitude, as the web plays them. This effect sits on the cue's own track and applies:
//!
//! - the fade-in, from 0 to `volume` over `fade_in` seconds;
//! - the scheduled fade-out over the last `fade_out` seconds of the edited length (not when
//!   looping), starting no earlier than the end of the fade-in;
//! - live volume changes;
//! - stop-with-fade: from the current level to 0 over N seconds, then silent.
//!
//! Timing: the sound starts at a `ClockTime` on the show clock. kira updates clocks before
//! mixing, and the sound and this effect both ask `Info::when_to_start` with the same time
//! in the same block, so the envelope starts on exactly the sound's first frame. While the
//! show track is paused, neither is processed, so everything freezes together.
//!
//! The audio thread never allocates, locks or logs here (CLAUDE.md rule 2): the handle talks
//! to the effect through atomics only.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

use kira::Frame;
use kira::clock::ClockTime;
use kira::effect::{Effect, EffectBuilder};
use kira::info::{Info, WhenToStart};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GainPhase {
    /// Before the start time (pre-wait).
    Waiting = 0,
    Playing = 1,
    /// Stopping with a fade.
    Fading = 2,
    /// A stop fade finished: nothing more will be heard.
    Silent = 3,
}

struct Shared {
    /// Target volume (linear amplitude), f32 bits.
    volume: AtomicU32,
    /// Stop-with-fade request: fade seconds as f32 bits; `stop_requested` publishes it.
    stop_seconds: AtomicU32,
    stop_requested: AtomicBool,
    /// Seconds played since the start (output time), f64 bits.
    elapsed: AtomicU64,
    phase: AtomicU8,
}

/// The engine's side of a `CueGain`.
pub struct CueGainHandle {
    shared: Arc<Shared>,
}

impl CueGainHandle {
    pub fn set_volume(&self, volume: f64) {
        self.shared
            .volume
            .store((volume as f32).to_bits(), Ordering::Release);
    }

    /// Fade from wherever the gain is now to silence over `seconds` (0 = at once).
    pub fn stop_with_fade(&self, seconds: f64) {
        self.shared
            .stop_seconds
            .store((seconds.max(0.0) as f32).to_bits(), Ordering::Release);
        self.shared.stop_requested.store(true, Ordering::Release);
    }

    pub fn elapsed(&self) -> f64 {
        f64::from_bits(self.shared.elapsed.load(Ordering::Acquire))
    }

    pub fn phase(&self) -> GainPhase {
        match self.shared.phase.load(Ordering::Acquire) {
            0 => GainPhase::Waiting,
            1 => GainPhase::Playing,
            2 => GainPhase::Fading,
            _ => GainPhase::Silent,
        }
    }
}

pub struct CueGainBuilder {
    pub volume: f64,
    pub fade_in: f64,
    pub fade_out: f64,
    /// Edited length in seconds.
    pub length: f64,
    pub looping: bool,
    /// When the sound starts; `None` = the first block processed.
    pub start: Option<ClockTime>,
    /// Seconds into the edited audio the sound starts at (an audition from a marker, a cue
    /// moved to another output): the envelope is taken from there, so fades sound exactly as
    /// they do in playback. 0 for a cue fired by GO.
    pub offset: f64,
}

impl EffectBuilder for CueGainBuilder {
    type Handle = CueGainHandle;

    fn build(self) -> (Box<dyn Effect>, Self::Handle) {
        let shared = Arc::new(Shared {
            volume: AtomicU32::new((self.volume as f32).to_bits()),
            stop_seconds: AtomicU32::new(0),
            stop_requested: AtomicBool::new(false),
            elapsed: AtomicU64::new(0f64.to_bits()),
            phase: AtomicU8::new(GainPhase::Waiting as u8),
        });
        let fade_out_start = fade_out_start(self.fade_in, self.fade_out, self.length, self.looping);
        let effect = CueGain {
            shared: shared.clone(),
            start: self.start,
            started: self.start.is_none(),
            frames_played: 0,
            fade_in: self.fade_in,
            fade_out_start,
            length: self.length,
            stop: None,
            last_gain: 0.0,
            offset: self.offset.max(0.0),
        };
        (Box::new(effect), CueGainHandle { shared })
    }
}

struct StopFade {
    from: f32,
    total: u64,
    done: u64,
}

struct CueGain {
    shared: Arc<Shared>,
    start: Option<ClockTime>,
    started: bool,
    /// Output frames since the sound started.
    frames_played: u64,
    fade_in: f64,
    fade_out_start: Option<f64>,
    length: f64,
    stop: Option<StopFade>,
    last_gain: f32,
    offset: f64,
}

/// Where the scheduled fade-out starts, if there is one. FORMAT.md 2.5.4: if fade-in and
/// fade-out overlap, the fade-out starts where the fade-in ends (web scheduleGain:
/// max(fade_in, length - fade_out)); a looping cue has no fade-out.
fn fade_out_start(fade_in: f64, fade_out: f64, length: f64, looping: bool) -> Option<f64> {
    (!looping && fade_out > 0.0).then(|| (length - fade_out).max(fade_in.min(length)))
}

/// The scheduled envelope (0..=1) at `t` seconds into the edited audio. The one definition of
/// a cue's fades: playback (`CueGain`) and the trim editor's gain line (`fade_curve`) both use it.
fn envelope(t: f64, fade_in: f64, fade_out_start: Option<f64>, length: f64) -> f64 {
    if let Some(fs) = fade_out_start
        && t >= fs
    {
        let span = length - fs;
        return if span > 0.0 {
            ((length - t) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
    }
    if fade_in > 0.0 && t < fade_in {
        return t / fade_in;
    }
    1.0
}

impl CueGain {
    fn envelope(&self, t: f64) -> f64 {
        envelope(t, self.fade_in, self.fade_out_start, self.length)
    }
}

/// A cue's edit and fades, for drawing its gain on the waveform.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct FadeShape {
    pub trim_start: f64,
    pub trim_end: Option<f64>,
    pub cuts: Vec<[f64; 2]>,
    pub fade_in: f64,
    pub fade_out: f64,
    #[serde(rename = "loop")]
    pub looping: bool,
}

/// The gain line for the trim editor (thorncue-spec#5): for each of `columns` equal slices of
/// the source file (`duration` seconds), the gain playback applies there, through trims and
/// cuts, or `None` where the audio doesn't play. Computed with playback's own envelope, so
/// what's drawn is what plays.
pub fn fade_curve(shape: &FadeShape, duration: f64, columns: usize) -> Vec<Option<f32>> {
    let segs = crate::edit::kept_segments(shape.trim_start, shape.trim_end, &shape.cuts, duration);
    let length: f64 = segs.iter().map(|(s, e)| e - s).sum();
    let fs = fade_out_start(shape.fade_in, shape.fade_out, length, shape.looping);
    (0..columns)
        .map(|x| {
            let t = (x as f64 + 0.5) / columns as f64 * duration;
            let mut played = 0.0;
            for &(s, e) in &segs {
                if t < s {
                    return None;
                }
                if t <= e {
                    played += t - s;
                    return Some(envelope(played, shape.fade_in, fs, length) as f32);
                }
                played += e - s;
            }
            None
        })
        .collect()
}

impl Effect for CueGain {
    fn process(&mut self, input: &mut [Frame], dt: f64, info: &Info) {
        if !self.started {
            if let Some(start) = self.start
                && info.when_to_start(start) == WhenToStart::Now
            {
                self.started = true;
                self.shared
                    .phase
                    .store(GainPhase::Playing as u8, Ordering::Release);
            } else {
                // The sound hasn't started either; keep whatever silence came in.
                for f in input.iter_mut() {
                    *f = Frame::ZERO;
                }
                return;
            }
        } else if self.shared.phase.load(Ordering::Relaxed) == GainPhase::Waiting as u8 {
            self.shared
                .phase
                .store(GainPhase::Playing as u8, Ordering::Release);
        }

        if self.stop.is_none() && self.shared.stop_requested.load(Ordering::Acquire) {
            let seconds = f32::from_bits(self.shared.stop_seconds.load(Ordering::Acquire)) as f64;
            self.stop = Some(StopFade {
                from: self.last_gain,
                total: (seconds / dt).round() as u64,
                done: 0,
            });
            self.shared
                .phase
                .store(GainPhase::Fading as u8, Ordering::Release);
        }

        let volume = f32::from_bits(self.shared.volume.load(Ordering::Acquire));
        for f in input.iter_mut() {
            let gain = match &mut self.stop {
                Some(s) => {
                    if s.done >= s.total {
                        0.0
                    } else {
                        let g = s.from * (1.0 - s.done as f32 / s.total as f32);
                        s.done += 1;
                        g
                    }
                }
                None => {
                    let t = self.offset + self.frames_played as f64 * dt;
                    volume * self.envelope(t) as f32
                }
            };
            *f *= gain;
            self.last_gain = gain;
            self.frames_played += 1;
        }
        if let Some(s) = &self.stop
            && s.done >= s.total
        {
            self.shared
                .phase
                .store(GainPhase::Silent as u8, Ordering::Release);
        }
        self.shared.elapsed.store(
            (self.frames_played as f64 * dt).to_bits(),
            Ordering::Release,
        );
    }
}
