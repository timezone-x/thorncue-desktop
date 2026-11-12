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
