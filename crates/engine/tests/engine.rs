//! Engine behaviour on the test backend (no sound card). Audio from docs/format/fixtures/audio.
//! `ladder-dc.wav`: 8 kHz mono, every sample in second n equals n * 1000 (16-bit).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use engine::kira::sound::static_sound::StaticSoundData;
use engine::kira::{AudioManager, AudioManagerSettings};
use engine::{
    Command, Core, Engine, EngineError, EngineStatus, Event, Loader, PlanCue, PrepState, RunPhase,
    ShowPlan, TestBackend, TestBackendSettings, kira_loader,
};

const RATE: u32 = 8000;
const BLOCK: usize = 128;

fn audio(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/format/fixtures/audio")
        .join(name)
}

fn ladder() -> PathBuf {
    audio("ladder-dc.wav")
}

/// The ladder's value in second n, as kira outputs it.
fn rung(n: u32) -> f32 {
    (n * 1000) as f32 / 32768.0
}

fn test_manager() -> Result<AudioManager<TestBackend>, String> {
    AudioManager::new(AudioManagerSettings {
        backend_settings: TestBackendSettings {
            sample_rate: RATE,
            ..Default::default()
        },
        ..Default::default()
    })
    .map_err(|e| match e {})
}

fn core_with(loader: Loader) -> (Core<TestBackend>, Receiver<Event>) {
    let (tx, rx) = unbounded();
    let core = Core::new(
        Box::new(test_manager),
        loader,
        Box::new(move |e| {
            let _ = tx.send(e);
        }),
    );
    (core, rx)
}

fn core() -> (Core<TestBackend>, Receiver<Event>) {
    core_with(kira_loader())
}

fn wait_ready(core: &mut Core<TestBackend>) {
    let t = Instant::now();
    loop {
        core.poll_prep();
        let s = core.snapshot();
        if s.prep_done == s.prep_total {
            return;
        }
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "audio never finished decoding"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Render `seconds` of audio in device-sized blocks, ticking the engine between blocks.
/// Returns the left channel.
fn render(core: &mut Core<TestBackend>, seconds: f64) -> Vec<f32> {
    let frames = (seconds * RATE as f64).round() as usize;
    let mut left = Vec::with_capacity(frames);
    let mut done = 0;
    while done < frames {
        let n = BLOCK.min(frames - done);
        let out = core.backend_mut().unwrap().render(n);
        left.extend(out.chunks(2).map(|f| f[0]));
        done += n;
        core.tick();
    }
    left
}

fn notices(rx: &Receiver<Event>) -> Vec<String> {
    rx.try_iter()
        .filter_map(|e| match e {
            Event::Notice(n) => Some(n.text),
            _ => None,
        })
        .collect()
}

fn plan(cues: Vec<PlanCue>) -> ShowPlan {
    ShowPlan { cues, standby: 0 }
}

fn silent(samples: &[f32]) -> bool {
    samples.iter().all(|s| s.abs() < 1e-6)
}

#[test]
fn go_plays_standby_at_its_volume_and_advances() {
    let (mut core, _rx) = core();
    let mut half = PlanCue::audio("b", ladder());
    half.volume = 0.5;
    core.handle(Command::LoadShow(plan(vec![
        PlanCue::audio("a", ladder()),
        half,
    ])));
    wait_ready(&mut core);
    let s = core.snapshot();
    assert!(s.prep.iter().all(|p| p.state == PrepState::Ready));
    assert!((s.prep[0].length.unwrap() - 10.0).abs() < 1e-6);
    assert_eq!(s.prep_total, 1, "cues sharing a file share one decode");

    core.handle(Command::Go);
    assert_eq!(core.standby(), 1);
    let out = render(&mut core, 2.5);
    assert_eq!(core.running_indexes(), vec![0]);
    // Exactly the file, from its first sample: second 2 holds 2000.
    assert!(
        (out[2 * RATE as usize + 100] - rung(2)).abs() < 1e-4,
        "{}",
        out[16100]
    );
    assert!((out[RATE as usize + 10] - rung(1)).abs() < 1e-4);

    // Q2 at volume 0.5, alone.
    core.handle(Command::StopAll { panic: false });
    render(&mut core, 0.05);
    core.handle(Command::Go);
    assert_eq!(core.standby(), 2);
    let out = render(&mut core, 1.5);
    assert!(
        (out[RATE as usize + 100] - 0.5 * rung(1)).abs() < 1e-3,
        "{}",
        out[RATE as usize + 100]
    );
}

#[test]
fn go_at_the_end_of_the_list_does_nothing_and_says_so() {
    let (mut core, rx) = core();
    core.handle(Command::LoadShow(ShowPlan {
        cues: vec![PlanCue::audio("a", ladder())],
        standby: 1,
    }));
    wait_ready(&mut core);
    core.handle(Command::Go);
    render(&mut core, 0.1);
    assert!(core.running_indexes().is_empty());
    assert_eq!(core.standby(), 1);
    assert!(
        notices(&rx)
            .iter()
            .any(|n| n.starts_with("End of the cue list"))
    );
}

#[test]
fn stop_all_stops_everything() {
    let (mut core, _rx) = core();
    core.handle(Command::LoadShow(plan(vec![
        PlanCue::audio("a", ladder()),
        PlanCue::audio("b", audio("tone-a440.wav")),
    ])));
    wait_ready(&mut core);
    core.handle(Command::Go);
    core.handle(Command::Go);
    let out = render(&mut core, 1.5);
    assert_eq!(core.running_indexes(), vec![0, 1]);
    assert!(!silent(&out[RATE as usize..]));

    core.handle(Command::StopAll { panic: false });
    assert!(
        core.running_indexes().is_empty(),
        "nothing shows as running after Stop all"
    );
    render(&mut core, 0.05); // the stop lands at the next block
    let after = render(&mut core, 1.0);
    assert!(silent(&after));
}

#[test]
fn pausing_the_show_track_freezes_child_tracks_and_resume_continues() {
    let (mut core, _rx) = core();
    core.handle(Command::LoadShow(plan(vec![PlanCue::audio("a", ladder())])));
    wait_ready(&mut core);
    core.handle(Command::Go);
    render(&mut core, 2.2);

    core.handle(Command::TogglePause);
    assert!(core.is_paused());
    render(&mut core, 0.05); // the pause lands at the next block
    let p1 = core.position_of(0).unwrap();
    let frozen = render(&mut core, 3.0);
    let p2 = core.position_of(0).unwrap();
    assert_eq!(p1, p2, "position must not advance while paused");
    assert!(silent(&frozen), "nothing may sound while paused");
    assert_eq!(
        core.running_indexes(),
        vec![0],
        "a paused cue is still running"
    );

    core.handle(Command::TogglePause);
    let out = render(&mut core, 0.5);
    let p3 = core.position_of(0).unwrap();
    assert!(
        (p3 - p2 - 0.5).abs() < 0.05,
        "resumes where it left off: {p2} -> {p3}"
    );
    // Still in second 2 of the file (2000), not jumped ahead by the 3 s pause.
    assert!(
        (out[out.len() - 1] - rung(2)).abs() < 1e-4,
        "{}",
        out[out.len() - 1]
    );
}

#[test]
fn pause_freezes_a_pre_wait() {
    let (mut core, _rx) = core();
    let mut c = PlanCue::audio("a", ladder());
    c.pre_wait = 0.5;
    core.handle(Command::LoadShow(plan(vec![c])));
    wait_ready(&mut core);
    core.handle(Command::Go);
    // ladder second 0 is silence, so watch the position rather than the samples.
    render(&mut core, 0.2);
    assert_eq!(core.position_of(0), Some(0.0), "still waiting");
    assert_eq!(core.snapshot().running[0].phase, RunPhase::Waiting);

    core.handle(Command::TogglePause);
    render(&mut core, 2.0);
    assert_eq!(
        core.position_of(0),
        Some(0.0),
        "the pre-wait must not run out while paused"
    );

    core.handle(Command::TogglePause);
    // 0.3 s of pre-wait left: still waiting after 0.25 s, playing after 0.35 s.
    // Tolerance: one kira block (128 frames = 16 ms at 8 kHz).
    render(&mut core, 0.25);
    assert_eq!(
        core.position_of(0),
        Some(0.0),
        "pre-wait resumed with its remaining time"
    );
    render(&mut core, 0.1);
    let p = core.position_of(0).unwrap();
    assert!(
        p > 0.0 && p <= 0.05 + 0.016,
        "started about 0.3 s after resume: position {p}"
    );
}

#[test]
fn pre_wait_delays_the_start() {
    let (mut core, _rx) = core();
    let mut c = PlanCue::audio("a", ladder());
    c.pre_wait = 1.0;
    core.handle(Command::LoadShow(plan(vec![c])));
    wait_ready(&mut core);
    core.handle(Command::Go);
    let out = render(&mut core, 3.5);
    // Without the wait, 2.5 s in would be second 2 of the file; with it, second 1.
    let i = (2.5 * RATE as f64) as usize;
    assert!((out[i] - rung(1)).abs() < 1e-4, "{}", out[i]);
}

#[test]
fn stop_all_during_a_pre_wait_cancels_it() {
    let (mut core, _rx) = core();
    let mut c = PlanCue::audio("a", audio("tone-a440.wav"));
    c.pre_wait = 0.5;
    core.handle(Command::LoadShow(plan(vec![c])));
    wait_ready(&mut core);
    core.handle(Command::Go);
    render(&mut core, 0.2);
    core.handle(Command::StopAll { panic: false });
    assert!(silent(&render(&mut core, 1.5)));
    assert!(core.running_indexes().is_empty());
}

/// A loader that waits until the test opens the gate (a slow decode we control).
fn gated_loader() -> (Loader, Arc<(Mutex<bool>, Condvar)>) {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let g = gate.clone();
    let loader: Loader = Arc::new(move |p: &Path| {
        let (lock, cv) = &*g;
        let mut open = lock.lock().unwrap();
        while !*open {
            open = cv.wait(open).unwrap();
        }
        StaticSoundData::from_file(p).map_err(|e| e.to_string())
    });
    (loader, gate)
}

fn open_gate(gate: &Arc<(Mutex<bool>, Condvar)>) {
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
}

#[test]
fn w1_go_on_a_decoding_cue_then_stop_all_never_starts_it() {
    let (loader, gate) = gated_loader();
    let (mut core, _rx) = core_with(loader);
    core.handle(Command::LoadShow(plan(vec![PlanCue::audio(
        "a",
        audio("tone-a440.wav"),
    )])));
    core.handle(Command::Go);
    assert_eq!(core.snapshot().running[0].phase, RunPhase::Starting);
    core.handle(Command::StopAll { panic: false });
    assert!(core.running_indexes().is_empty());

    open_gate(&gate);
    wait_ready(&mut core);
    let out = render(&mut core, 1.0);
    assert!(silent(&out), "the cancelled GO must never sound");
    assert!(core.running_indexes().is_empty());
}

#[test]
fn go_on_a_decoding_cue_starts_it_the_moment_it_is_ready() {
    let (loader, gate) = gated_loader();
    let (mut core, _rx) = core_with(loader);
    core.handle(Command::LoadShow(plan(vec![PlanCue::audio(
        "a",
        audio("tone-a440.wav"),
    )])));
    core.handle(Command::Go);
    assert_eq!(
        core.standby(),
        1,
        "standby moves on at once, as for a ready cue"
    );
    open_gate(&gate);
    wait_ready(&mut core);
    let out = render(&mut core, 0.5);
    assert!(!silent(&out));
    assert_eq!(core.running_indexes(), vec![0]);
}

#[test]
fn missing_audio_is_reported_and_standby_still_advances() {
    let (mut core, rx) = core();
    let mut none = PlanCue::audio("a", ladder());
    none.audio = None;
    core.handle(Command::LoadShow(plan(vec![
        none,
        PlanCue::audio("b", audio("no-such-file.wav")),
        PlanCue::audio("c", audio("not-audio.txt")),
    ])));
    wait_ready(&mut core);
    let s = core.snapshot();
    assert_eq!(s.prep[0].state, PrepState::Missing);
    assert_eq!(s.prep[1].state, PrepState::Missing);
    assert!(matches!(s.prep[2].state, PrepState::Failed(_)));
    for _ in 0..3 {
        core.handle(Command::Go);
    }
    assert_eq!(core.standby(), 3);
    assert!(core.running_indexes().is_empty());
    let n = notices(&rx);
    assert_eq!(
        n.iter().filter(|t| t.contains("can't play")).count(),
        3,
        "{n:?}"
    );
}

#[test]
fn go_on_a_running_cue_restarts_it() {
    let (mut core, _rx) = core();
    core.handle(Command::LoadShow(plan(vec![PlanCue::audio("a", ladder())])));
    wait_ready(&mut core);
    core.handle(Command::Go);
    render(&mut core, 2.5);
    core.handle(Command::SetStandby(0));
    core.handle(Command::Go);
    render(&mut core, 0.5);
    assert_eq!(core.running_indexes(), vec![0], "one instance, not two");
    assert!(core.position_of(0).unwrap() < 0.6);
}

#[test]
fn standby_moves_and_jump_plays() {
    let (mut core, rx) = core();
    core.handle(Command::LoadShow(plan(vec![
        PlanCue::audio("a", ladder()),
        PlanCue::audio("b", ladder()),
        PlanCue::audio("c", ladder()),
    ])));
    wait_ready(&mut core);
    core.handle(Command::MoveStandby(1));
    core.handle(Command::MoveStandby(5));
    assert_eq!(core.standby(), 2, "arrows stop at the last cue");
    core.handle(Command::MoveStandby(-9));
    assert_eq!(core.standby(), 0);
    core.handle(Command::JumpAndPlay(1));
    assert_eq!(core.standby(), 2);
    render(&mut core, 0.1);
    assert_eq!(core.running_indexes(), vec![1]);
    core.handle(Command::JumpAndPlay(7));
    assert!(
        notices(&rx)
            .iter()
            .any(|n| n == "There is no Q8. This show has 3 cues.")
    );
}

#[test]
fn a_device_error_shows_device_lost() {
    let (mut core, rx) = core();
    core.backend_mut()
        .unwrap()
        .inject_device_error("device unplugged");
    core.tick();
    assert_eq!(core.snapshot().status, EngineStatus::DeviceLost);
    assert!(
        notices(&rx)
            .iter()
            .any(|n| n.contains("Restart audio engine"))
    );
    core.handle(Command::RestartAudio);
    assert_eq!(core.snapshot().status, EngineStatus::Ok);
}

#[test]
fn a_cue_that_ends_by_itself_leaves_the_running_list() {
    let (mut core, _rx) = core();
    core.handle(Command::LoadShow(plan(vec![PlanCue::audio(
        "a",
        audio("tone-a440.wav"),
    )])));
    wait_ready(&mut core);
    core.handle(Command::Go);
    render(&mut core, 1.0);
    assert_eq!(core.running_indexes(), vec![0]);
    render(&mut core, 1.2);
    assert!(core.running_indexes().is_empty());
}

#[test]
fn restart_refuses_commands_until_rebuilt() {
    // The second factory call (the restart) blocks until the test lets it finish.
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let g = gate.clone();
    let mut calls = 0;
    let factory = move || {
        calls += 1;
        if calls > 1 {
            let (lock, cv) = &*g;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
        }
        test_manager()
    };
    let engine = Engine::start::<TestBackend>(factory, kira_loader(), |_| {});
    engine
        .send(Command::LoadShow(plan(vec![PlanCue::audio("a", ladder())])))
        .unwrap();

    engine.send(Command::RestartAudio).unwrap();
    assert_eq!(engine.snapshot().status, EngineStatus::Restarting);
    assert_eq!(engine.send(Command::Go), Err(EngineError::Restarting));
    assert_eq!(
        engine.send(Command::TogglePause),
        Err(EngineError::Restarting)
    );
    assert_eq!(
        engine.send(Command::RestartAudio),
        Err(EngineError::Restarting)
    );
    assert!(
        engine.send(Command::StopAll { panic: true }).is_ok(),
        "Stop all always wins"
    );

    open_gate(&gate);
    let t = Instant::now();
    while engine.is_restarting() {
        assert!(t.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(engine.send(Command::Go).is_ok());
    let t = Instant::now();
    while engine.snapshot().status != EngineStatus::Ok {
        assert!(t.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn snapshot_serialises_for_the_ui() {
    let (mut core, _rx) = core();
    core.handle(Command::LoadShow(plan(vec![PlanCue::audio("a", ladder())])));
    wait_ready(&mut core);
    let v = serde_json::to_value(core.snapshot()).unwrap();
    assert_eq!(v["prep"][0]["state"], "ready");
    assert_eq!(v["status"], "ok");
}

#[test]
fn closing_the_engine_does_not_wait_for_a_decode() {
    let (loader, _gate) = gated_loader(); // never opened: the decode never finishes
    let (mut core, _rx) = core_with(loader);
    core.handle(Command::LoadShow(plan(vec![PlanCue::audio("a", ladder())])));
    std::thread::sleep(Duration::from_millis(50));
    core.poll_prep();
    assert_eq!(core.snapshot().prep[0].state, PrepState::Decoding);
    let t = Instant::now();
    drop(core);
    assert!(t.elapsed() < Duration::from_secs(1));
}

fn stall_watching_core() -> (Core<TestBackend>, Receiver<Event>) {
    let (tx, rx) = unbounded();
    let factory = || {
        AudioManager::new(AudioManagerSettings {
            backend_settings: TestBackendSettings {
                sample_rate: RATE,
                watch_for_stalls: true,
            },
            ..Default::default()
        })
        .map_err(|e| match e {})
    };
    let core = Core::new(
        Box::new(factory),
        kira_loader(),
        Box::new(move |e| {
            let _ = tx.send(e);
        }),
    );
    (core, rx)
}

/// Behave like a live device for `seconds` of wall-clock time.
fn run_live(core: &mut Core<TestBackend>, seconds: f64) {
    let t = Instant::now();
    while t.elapsed().as_secs_f64() < seconds {
        render(core, 0.02);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn an_output_that_stops_pulling_audio_shows_device_lost_and_clears_when_it_returns() {
    let (mut core, rx) = stall_watching_core();
    // A just-opened device gets a grace period before its first callback.
    std::thread::sleep(Duration::from_millis(1000));
    core.tick();
    assert_eq!(
        core.snapshot().status,
        EngineStatus::Ok,
        "no false alarm while the output opens"
    );
    run_live(&mut core, 0.5);
    // A short hiccup (PulseAudio under load) is not an alarm.
    std::thread::sleep(Duration::from_millis(800));
    core.tick();
    assert_eq!(
        core.snapshot().status,
        EngineStatus::Ok,
        "a hiccup is not a lost device"
    );
    run_live(&mut core, 0.2);
    // The device stops pulling audio.
    std::thread::sleep(Duration::from_millis(2100));
    core.tick();
    assert_eq!(core.snapshot().status, EngineStatus::DeviceLost);
    assert!(
        notices(&rx)
            .iter()
            .any(|n| n.contains("Restart audio engine"))
    );
    // Audio flows steadily again: the alarm clears itself.
    run_live(&mut core, 1.3);
    assert_eq!(core.snapshot().status, EngineStatus::Ok);
    assert!(
        notices(&rx)
            .iter()
            .any(|n| n == "The audio output is working again.")
    );
}

#[test]
fn a_reported_disconnect_stays_until_restart() {
    let (mut core, _rx) = stall_watching_core();
    run_live(&mut core, 0.2);
    core.backend_mut()
        .unwrap()
        .inject_device_error("device unplugged");
    core.tick();
    assert_eq!(core.snapshot().status, EngineStatus::DeviceLost);
    // Audio may be flowing again, out of the wrong speakers: stay lost.
    run_live(&mut core, 1.3);
    assert_eq!(core.snapshot().status, EngineStatus::DeviceLost);
    core.handle(Command::RestartAudio);
    assert_eq!(core.snapshot().status, EngineStatus::Ok);
}

// ---------------------------------------------------------------------------
// M4: full playback semantics (FORMAT.md 2.5), sample-checked against ladder-dc.wav.
// ---------------------------------------------------------------------------

use engine::Options;

/// The DC ladder's sample for source second `t` (as kira outputs it).
fn ladder_at(t: f64) -> f32 {
    rung(t.floor() as u32)
}

fn dc(id: &str) -> PlanCue {
    PlanCue::audio(id, ladder())
}

/// Index of the first sample louder than `eps`.
fn first_sound(out: &[f32]) -> Option<usize> {
    out.iter().position(|s| s.abs() > 1e-6)
}

fn ready_core(cues: Vec<PlanCue>) -> (Core<TestBackend>, Receiver<Event>) {
    let (mut core, rx) = core();
    core.handle(Command::LoadShow(plan(cues)));
    wait_ready(&mut core);
    (core, rx)
}

#[test]
fn fixture_02_q10_trim_2_to_9_cut_4_to_6_is_exact() {
    // FORMAT fixtures README: 2 s of 2000/3000, then 3 s of 6000/7000/8000, with 4 ms ramps
    // only at the join.
    let mut q10 = dc("q10");
    q10.trim_start = 2.0;
    q10.trim_end = Some(9.0);
    q10.cuts = vec![[4.0, 6.0]];
    let (mut core, _rx) = ready_core(vec![q10]);
    assert!((core.snapshot().prep[0].length.unwrap() - 5.0).abs() < 1e-9);
    core.handle(Command::Go);
    let out = render(&mut core, 5.5);
    let r = RATE as usize;
    let ramp = 32; // 4 ms at 8 kHz
    let join = 2 * r;
    for (n, &got) in out.iter().enumerate().take(5 * r) {
        // Edited time -> source time.
        let src = if n < join {
            2.0 + n as f64 / RATE as f64
        } else {
            6.0 + (n - join) as f64 / RATE as f64
        };
        let mut want = ladder_at(src);
        if n < join && n >= join - ramp {
            want *= (join - 1 - n) as f32 / ramp as f32; // ramp down into the join
        } else if n >= join && n < join + ramp {
            want *= (n - join) as f32 / ramp as f32; // ramp up out of it
        }
        assert!(
            (got - want).abs() < 1e-5,
            "sample {n}: got {got}, want {want}"
        );
    }
    // Ends after exactly 5 s.
    assert!(silent(&out[5 * r..]));
    assert!(core.running_indexes().is_empty());
}

#[test]
fn fixture_02_q01_edited_length_is_5_5_s() {
    // Trim 1.0-8.0, cuts 3-4 and 5.5-6 on the pitch ladder (MP3).
    let mut q01 = PlanCue::audio("q01", audio("ladder-10s.mp3"));
    q01.trim_start = 1.0;
    q01.trim_end = Some(8.0);
    q01.cuts = vec![[3.0, 4.0], [5.5, 6.0]];
    let (core, _rx) = ready_core(vec![q01]);
    let len = core.snapshot().prep[0].length.unwrap();
    assert!((len - 5.5).abs() < 0.001, "{len}");
}

#[test]
fn trims_alone_play_the_slice() {
    let mut c = dc("a");
    c.trim_start = 3.0;
    c.trim_end = Some(5.0);
    let (mut core, _rx) = ready_core(vec![c]);
    core.handle(Command::Go);
    let out = render(&mut core, 2.5);
    assert!((out[100] - rung(3)).abs() < 1e-5);
    assert!((out[RATE as usize + 100] - rung(4)).abs() < 1e-5);
    assert!(silent(&out[2 * RATE as usize + 10..]));
}

/// Gain applied at output sample n, against the DC source played from second 1 (value 1000)
/// so source/out is exactly the gain. trim 1-3: 1000 for 1 s then 2000 for 1 s.
fn gain_curve(fade_in: f64, fade_out: f64) -> Vec<f32> {
    let mut c = dc("a");
    c.trim_start = 1.0;
    c.trim_end = Some(3.0);
    c.fade_in = fade_in;
    c.fade_out = fade_out;
    let (mut core, _rx) = ready_core(vec![c]);
    core.handle(Command::Go);
    let out = render(&mut core, 2.0);
    out.iter()
        .enumerate()
        .map(|(n, s)| s / ladder_at(1.0 + n as f64 / RATE as f64))
        .collect()
}

#[test]
fn fades_are_linear_in_amplitude_and_land_on_the_right_samples() {
    let g = gain_curve(0.5, 0.5);
    let r = RATE as f64;
    for (n, &got) in g.iter().enumerate() {
        let t = n as f64 / r;
        let want = if t < 0.5 {
            t / 0.5
        } else if t >= 1.5 {
            (2.0 - t) / 0.5
        } else {
            1.0
        };
        assert!(
            (got as f64 - want).abs() < 1e-4,
            "sample {n} (t={t}): gain {got}, want {want}"
        );
    }
    // Halfway through the fade-in the gain is 0.5 (linear), not 0.5 in dB terms.
    assert!((g[(0.25 * r) as usize] - 0.5).abs() < 1e-4);
}

#[test]
fn overlapping_fades_start_the_fade_out_where_the_fade_in_ends() {
    // Length 2 s, fade in 1.5 s, fade out 1.0 s: the fade-out runs 1.5 -> 2.0 (FORMAT.md 2.5.4).
    let g = gain_curve(1.5, 1.0);
    let at = |t: f64| g[(t * RATE as f64) as usize] as f64;
    assert!((at(0.75) - 0.5).abs() < 1e-3);
    assert!((at(1.5) - 1.0).abs() < 1e-3);
    assert!((at(1.75) - 0.5).abs() < 1e-3);
}

#[test]
fn volume_scales_linearly_and_can_go_above_one() {
    let mut c = dc("a");
    c.trim_start = 1.0;
    c.volume = 2.0;
    let (mut core, _rx) = ready_core(vec![c]);
    core.handle(Command::Go);
    let out = render(&mut core, 0.5);
    assert!((out[100] - 2.0 * rung(1)).abs() < 1e-5, "{}", out[100]);
}

#[test]
fn pre_wait_delays_the_start_and_the_fade_in_starts_with_the_sound() {
    let mut c = dc("a");
    c.trim_start = 1.0;
    c.pre_wait = 0.25;
    c.fade_in = 0.1;
    let (mut core, _rx) = ready_core(vec![c]);
    core.handle(Command::Go);
    let out = render(&mut core, 1.0);
    let start = first_sound(&out).unwrap();
    let want = (0.25 * RATE as f64) as usize;
    // kira advances clocks before mixing a block, so a start lands within one block of the
    // exact sample (the agreed tolerance), possibly early.
    assert!(
        start + BLOCK >= want && start <= want + BLOCK,
        "started at sample {start}, want {want} (± one block)"
    );
    // The envelope begins on the sound's first sample (gain 0 there, so the first audible
    // sample is the second): sample k of the sound is at k / 800 of full level.
    let first = start - 1;
    for k in 0..400 {
        let g = out[first + k] / rung(1);
        assert!((g - k as f32 / 800.0).abs() < 1e-5, "k={k}: {g}");
    }
    assert!(first + BLOCK >= want && first <= want + BLOCK);
}

#[test]
fn continue_after_end_fires_the_next_cue() {
    let mut a = dc("a");
    a.trim_start = 1.0;
    a.trim_end = Some(1.5);
    a.continue_after = true;
    let mut b = dc("b");
    b.trim_start = 3.0;
    let (mut core, _rx) = ready_core(vec![a, b]);
    core.handle(Command::Go);
    let out = render(&mut core, 1.2);
    assert_eq!(core.running_indexes(), vec![1]);
    assert_eq!(core.standby(), 2, "as if GO had been pressed on Q2");
    // Q2 starts within a tick and a block of Q1's end.
    let q2 = out
        .iter()
        .position(|&s| (s - rung(3)).abs() < 1e-5)
        .unwrap();
    let end = RATE as usize / 2;
    assert!(
        q2 >= end && q2 < end + 3 * BLOCK,
        "Q2 began at {q2}, Q1 ended at {end}"
    );
}

#[test]
fn stopped_cues_never_continue() {
    let mut a = dc("a");
    a.continue_after = true;
    let (mut core, _rx) = ready_core(vec![a, dc("b")]);
    core.handle(Command::Go);
    render(&mut core, 0.5);
    core.handle(Command::StopAll { panic: false });
    render(&mut core, 1.0);
    assert!(core.running_indexes().is_empty());
    assert_eq!(core.standby(), 1);
}

#[test]
fn looping_is_seamless_ignores_fade_out_and_never_continues() {
    let mut a = dc("a");
    a.trim_start = 1.0;
    a.trim_end = Some(3.0);
    a.looping = true;
    a.fade_out = 1.0; // ignored while looping
    a.continue_after = true;
    let (mut core, _rx) = ready_core(vec![a, dc("b")]);
    core.handle(Command::Go);
    let out = render(&mut core, 5.0);
    let r = RATE as usize;
    for (n, &got) in out.iter().enumerate().take(5 * r) {
        let want = ladder_at(1.0 + ((n % (2 * r)) as f64 / RATE as f64));
        assert!((got - want).abs() < 1e-5, "sample {n}: {got} vs {want}");
    }
    assert_eq!(core.running_indexes(), vec![0]);
    let p = core.positions();
    assert_eq!(p[0].remaining, None);
}

#[test]
fn silence_stops_everything_then_waits_and_continues() {
    let mut s = PlanCue::audio("s", ladder());
    s.kind = engine::CueKind::Silence;
    s.audio = None;
    s.duration = 0.5;
    s.continue_after = true;
    let mut b = dc("b");
    b.trim_start = 3.0;
    let mut a = dc("a");
    a.trim_start = 1.0;
    let (mut core, _rx) = ready_core(vec![a, s, b]);
    core.handle(Command::Go); // Q1 plays
    render(&mut core, 0.5);
    core.handle(Command::Go); // silence: stops Q1 at once
    let out = render(&mut core, 1.0);
    let half = RATE as usize / 2;
    assert!(silent(&out[BLOCK..half]), "silence for 0.5 s");
    let q3 = out
        .iter()
        .position(|&s| (s - rung(3)).abs() < 1e-5)
        .unwrap();
    assert!(q3 >= half && q3 < half + 3 * BLOCK, "Q3 began at {q3}");
    assert_eq!(core.running_indexes(), vec![2]);
}

#[test]
fn fade_out_cue_fades_everything_linearly_then_stops_and_continues() {
    let mut f = PlanCue::audio("f", ladder());
    f.kind = engine::CueKind::Fadeout;
    f.audio = None;
    f.duration = 1.0;
    f.continue_after = true;
    let mut a = dc("a");
    a.trim_start = 1.0;
    a.continue_after = true; // must not fire: a is stopped by the fade-out cue
    let mut c = dc("c");
    c.trim_start = 5.0;
    let (mut core, _rx) = ready_core(vec![a, f, c]);
    core.handle(Command::Go);
    render(&mut core, 0.25);
    core.handle(Command::Go); // fade everything over 1 s
    let out = render(&mut core, 0.75);
    // From full level to 0, linear: at 0.5 s in, half.
    let mid = RATE as usize / 2;
    let g = out[mid] / ladder_at(1.25 + 0.5);
    assert!((g - 0.5).abs() < 0.02, "gain {g} half way through the fade");
    let out = render(&mut core, 0.5);
    // Q1 is gone; the fade-out cue's own continue fires Q3 (value 5000), not Q2.
    assert!(out.iter().any(|&s| (s - rung(5)).abs() < 1e-5));
    assert_eq!(core.running_indexes(), vec![2]);
}

#[test]
fn length_zero_cues_end_at_once_and_continue() {
    // Fixture 02 q12: cuts remove the whole file; continue fires into the next cue.
    let mut z = dc("z");
    z.cuts = vec![[0.0, 10.0]];
    z.continue_after = true;
    let mut b = dc("b");
    b.trim_start = 2.0;
    let (mut core, _rx) = ready_core(vec![z, b]);
    assert_eq!(core.snapshot().prep[0].length, Some(0.0));
    core.handle(Command::Go);
    let out = render(&mut core, 0.2);
    assert_eq!(core.running_indexes(), vec![1]);
    let q2 = out
        .iter()
        .position(|&s| (s - rung(2)).abs() < 1e-5)
        .unwrap();
    assert!(q2 < 3 * BLOCK, "Q2 began at {q2}");
}

#[test]
fn master_volume_scales_everything() {
    let mut a = dc("a");
    a.trim_start = 1.0;
    let (mut core, _rx) = ready_core(vec![a]);
    core.handle(Command::SetMaster(0.5));
    core.handle(Command::Go);
    let out = render(&mut core, 0.5);
    assert!(
        (out[RATE as usize / 4] - 0.5 * rung(1)).abs() < 1e-4,
        "{}",
        out[RATE as usize / 4]
    );
    assert_eq!(core.snapshot().master, 0.5);
}

#[test]
fn live_volume_change_takes_effect_while_playing() {
    let mut a = dc("a");
    a.trim_start = 1.0;
    let (mut core, _rx) = ready_core(vec![a]);
    core.handle(Command::Go);
    render(&mut core, 0.2);
    core.handle(Command::SetCueVolume {
        index: 0,
        volume: 0.25,
    });
    let out = render(&mut core, 0.2);
    assert!((out[out.len() - 1] - 0.25 * rung(1)).abs() < 1e-5);
}

#[test]
fn go_lockout_ignores_a_double_tap() {
    let (mut core, _rx) = ready_core(vec![dc("a"), dc("b"), dc("c")]);
    core.handle(Command::SetOptions(Options {
        go_lockout_ms: 300,
        stop_fade: 0.0,
    }));
    core.handle(Command::Go);
    core.handle(Command::Go);
    assert_eq!(core.standby(), 1, "second GO within 300 ms ignored");
    std::thread::sleep(Duration::from_millis(320));
    core.handle(Command::Go);
    assert_eq!(core.standby(), 2);
}

#[test]
fn stop_all_fade_fades_and_panic_cuts() {
    let mut a = dc("a");
    a.trim_start = 1.0;
    let (mut core, _rx) = ready_core(vec![a.clone()]);
    core.handle(Command::SetOptions(Options {
        go_lockout_ms: 0,
        stop_fade: 1.0,
    }));
    core.handle(Command::Go);
    render(&mut core, 0.2);
    core.handle(Command::StopAll { panic: false });
    assert_eq!(core.snapshot().running[0].phase, RunPhase::Fading);
    let out = render(&mut core, 0.5);
    let g = out[out.len() - 1] / rung(1);
    assert!(
        (g - 0.5).abs() < 0.03,
        "half way through a 1 s stop fade: {g}"
    );
    // Panic during the fade: silence at the next block.
    core.handle(Command::StopAll { panic: true });
    render(&mut core, 0.05);
    assert!(silent(&render(&mut core, 0.5)));
    assert!(core.running_indexes().is_empty());
}

#[test]
fn pause_freezes_silence_timers_too() {
    let mut s = PlanCue::audio("s", ladder());
    s.kind = engine::CueKind::Silence;
    s.audio = None;
    s.duration = 0.5;
    let (mut core, _rx) = ready_core(vec![s]);
    core.handle(Command::Go);
    render(&mut core, 0.2);
    core.handle(Command::TogglePause);
    render(&mut core, 2.0);
    assert_eq!(
        core.running_indexes(),
        vec![0],
        "the silence must not run out while paused"
    );
    core.handle(Command::TogglePause);
    render(&mut core, 0.4);
    assert!(core.running_indexes().is_empty());
}

// ---------------------------------------------------------------------------
// M6: edits while the show is open (UpdatePlan)
// ---------------------------------------------------------------------------

/// A loader that counts decodes per file name.
fn counting_loader() -> (Loader, Arc<Mutex<Vec<String>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = log.clone();
    let loader: Loader = Arc::new(move |p: &Path| {
        l.lock()
            .unwrap()
            .push(p.file_name().unwrap().to_string_lossy().to_string());
        StaticSoundData::from_file(p).map_err(|e| e.to_string())
    });
    (loader, log)
}

fn update(core: &mut Core<TestBackend>, cues: Vec<PlanCue>) {
    core.handle(Command::UpdatePlan(ShowPlan { cues, standby: 0 }));
}

#[test]
fn editing_keeps_running_cues_and_standby_on_their_cues() {
    let mut a = dc("a");
    a.trim_start = 1.0;
    let (mut core, _rx) = ready_core(vec![a.clone(), dc("b"), dc("c")]);
    core.handle(Command::Go); // a runs, standby on b
    render(&mut core, 0.5);
    let mut renamed = a.clone();
    renamed.name = "Renamed".into();
    let mut new = PlanCue::audio("n", ladder());
    new.name = "Inserted".into();
    update(&mut core, vec![new, renamed, dc("b"), dc("c")]);
    assert_eq!(
        core.running_indexes(),
        vec![1],
        "a moved to index 1 and is still running"
    );
    assert_eq!(core.standby(), 2, "standby still on b");
    let out = render(&mut core, 0.5);
    assert!(
        (out[100] - rung(1)).abs() < 1e-5,
        "a kept playing without a restart"
    );
}

#[test]
fn a_volume_edit_changes_a_playing_cue_at_once() {
    let mut a = dc("a");
    a.trim_start = 1.0;
    let (mut core, _rx) = ready_core(vec![a.clone()]);
    core.handle(Command::Go);
    render(&mut core, 0.3);
    a.volume = 0.5;
    update(&mut core, vec![a]);
    let out = render(&mut core, 0.2);
    assert!((out[out.len() - 1] - 0.5 * rung(1)).abs() < 1e-5);
}

#[test]
fn deleting_a_running_cue_stops_it() {
    let (mut core, _rx) = ready_core(vec![dc("a"), dc("b")]);
    core.handle(Command::JumpAndPlay(1));
    render(&mut core, 1.2);
    update(&mut core, vec![dc("a")]);
    render(&mut core, 0.05);
    assert!(core.running_indexes().is_empty());
    assert!(silent(&render(&mut core, 0.5)));
    assert_eq!(
        core.standby(),
        1,
        "standby was past the end of the list; it stays at the end"
    );
}

#[test]
fn a_trim_edit_renders_without_decoding_again() {
    let (loader, log) = counting_loader();
    let (mut core, _rx) = core_with(loader);
    core.handle(Command::LoadShow(plan(vec![dc("a")])));
    wait_ready(&mut core);
    let mut a = dc("a");
    a.trim_start = 3.0;
    a.trim_end = Some(4.0);
    update(&mut core, vec![a]);
    // Ready again once the render lands.
    let t = Instant::now();
    while core.snapshot().prep[0].length.is_none() {
        core.poll_prep();
        assert!(t.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!((core.snapshot().prep[0].length.unwrap() - 1.0).abs() < 1e-9);
    assert_eq!(log.lock().unwrap().len(), 1, "decoded once only");
    core.handle(Command::Go);
    let out = render(&mut core, 1.2);
    assert!((out[100] - rung(3)).abs() < 1e-5);
    assert!(silent(&out[RATE as usize + 10..]));
}

#[test]
fn adding_a_cue_decodes_only_the_new_file() {
    let (loader, log) = counting_loader();
    let (mut core, _rx) = core_with(loader);
    core.handle(Command::LoadShow(plan(vec![dc("a")])));
    wait_ready(&mut core);
    update(
        &mut core,
        vec![dc("a"), PlanCue::audio("t", audio("tone-a440.wav"))],
    );
    wait_ready(&mut core);
    let l = log.lock().unwrap().clone();
    assert_eq!(
        l,
        vec!["ladder-dc.wav".to_string(), "tone-a440.wav".to_string()]
    );
    assert_eq!(core.snapshot().prep[1].state, PrepState::Ready);
}

#[test]
fn go_while_a_new_edit_renders_then_stop_all_never_starts_it() {
    let (mut core, _rx) = ready_core(vec![dc("a")]);
    let mut a = dc("a");
    a.cuts = vec![[2.0, 3.0]];
    update(&mut core, vec![a]);
    // Don't poll: the render is in flight. GO waits for it.
    core.handle(Command::Go);
    assert_eq!(core.snapshot().running[0].phase, RunPhase::Starting);
    core.handle(Command::StopAll { panic: false });
    let t = Instant::now();
    while core.snapshot().prep[0].length.is_none() {
        core.poll_prep();
        assert!(t.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(silent(&render(&mut core, 0.5)));
    assert!(core.running_indexes().is_empty());
}
