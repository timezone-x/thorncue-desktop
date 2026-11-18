//! Export one cue as WAV (web "Export WAV"), checked against `ladder-dc.wav`: 8 kHz mono, every
//! sample in second n equals n * 1000 (16-bit), so trims, cuts, volume and fades are checkable
//! to the sample.

use std::path::{Path, PathBuf};

use engine::kira::sound::static_sound::StaticSoundData;
use engine::{CueRender, FadeShape, cue_wav};

fn audio(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/format/fixtures/audio")
        .join(name)
}

fn load(name: &str) -> StaticSoundData {
    StaticSoundData::from_file(audio(name)).unwrap()
}

fn render(
    trim: (f64, Option<f64>),
    cuts: &[[f64; 2]],
    fades: (f64, f64),
    looping: bool,
    volume: f64,
) -> CueRender {
    CueRender {
        shape: FadeShape {
            trim_start: trim.0,
            trim_end: trim.1,
            cuts: cuts.to_vec(),
            fade_in: fades.0,
            fade_out: fades.1,
            looping,
        },
        volume,
    }
}

struct Wav {
    channels: u16,
    rate: u32,
    samples: Vec<i16>,
}

fn parse(b: &[u8]) -> Wav {
    let u16at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let u32at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    assert_eq!(&b[0..4], b"RIFF");
    assert_eq!(u32at(4) as usize, b.len() - 8);
    assert_eq!(&b[8..16], b"WAVEfmt ");
    assert_eq!(u16at(20), 1, "PCM");
    assert_eq!(u16at(34), 16, "16-bit");
    let channels = u16at(22);
    let rate = u32at(24);
    assert_eq!(u32at(28), rate * channels as u32 * 2);
    assert_eq!(&b[36..40], b"data");
    assert_eq!(u32at(40) as usize, b.len() - 44);
    let samples = b[44..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    Wav {
        channels,
        rate,
        samples,
    }
}

const RATE: usize = 8000;

fn near(a: i16, b: f64) {
    assert!((a as f64 - b).abs() <= 1.0, "{a} is not {b}");
}

#[test]
fn trims_cuts_and_volume_are_applied() {
    // Source 2-9 s minus 4-6 s: 2 s + 3 s = 5 s, at half volume.
    let w = parse(
        &cue_wav(
            &load("ladder-dc.wav"),
            &render((2.0, Some(9.0)), &[[4.0, 6.0]], (0.0, 0.0), false, 0.5),
        )
        .unwrap(),
    );
    assert_eq!(w.channels, 1, "mono in, mono out");
    assert_eq!(w.rate, 8000);
    assert_eq!(w.samples.len(), 5 * RATE);
    near(w.samples[0], 2000.0 * 0.5); // source second 2
    near(w.samples[RATE + RATE / 2], 3000.0 * 0.5); // source 3.5 s
    near(w.samples[3 * RATE + RATE / 2], 7000.0 * 0.5); // 3.5 s in = source 7.5 s (after the cut)
    near(*w.samples.last().unwrap(), 8000.0 * 0.5); // source just before 9 s
}

#[test]
fn a_trim_alone_exports_only_the_trimmed_part() {
    let w = parse(
        &cue_wav(
            &load("ladder-dc.wav"),
            &render((1.0, Some(3.0)), &[], (0.0, 0.0), false, 1.0),
        )
        .unwrap(),
    );
    assert_eq!(w.samples.len(), 2 * RATE);
    near(w.samples[0], 1000.0);
    near(w.samples[RATE], 2000.0);
}

#[test]
fn fades_follow_playback() {
    // Source 4-8 s (4 s long), fade in 1 s, fade out 2 s: linear in amplitude.
    let w = parse(
        &cue_wav(
            &load("ladder-dc.wav"),
            &render((4.0, Some(8.0)), &[], (1.0, 2.0), false, 1.0),
        )
        .unwrap(),
    );
    assert_eq!(w.samples.len(), 4 * RATE);
    assert_eq!(w.samples[0], 0, "fade in starts silent");
    near(w.samples[RATE / 2], 4000.0 * 0.5);
    near(w.samples[RATE + RATE / 2], 5000.0); // between the fades: full
    near(w.samples[3 * RATE], 7000.0 * 0.5); // halfway through the fade out
    assert!(w.samples.last().unwrap().abs() <= 1, "fade out ends silent");
}

#[test]
fn a_looping_cue_is_one_pass_without_its_fade_out() {
    // As playback: a looping cue never fades out on its own.
    let w = parse(
        &cue_wav(
            &load("ladder-dc.wav"),
            &render((0.0, Some(2.0)), &[], (0.0, 1.0), true, 1.0),
        )
        .unwrap(),
    );
    assert_eq!(w.samples.len(), 2 * RATE);
    near(*w.samples.last().unwrap(), 1000.0);
}

#[test]
fn volume_above_100_percent_is_applied() {
    let w = parse(
        &cue_wav(
            &load("ladder-dc.wav"),
            &render((9.0, None), &[], (0.0, 0.0), false, 2.0),
        )
        .unwrap(),
    );
    near(w.samples[0], 18000.0);
}

#[test]
fn stereo_stays_stereo() {
    let w = parse(
        &cue_wav(
            &load("stereo-48k.wav"),
            &render((0.0, None), &[], (0.0, 0.0), false, 1.0),
        )
        .unwrap(),
    );
    assert_eq!(w.channels, 2);
    assert_eq!(w.rate, 48000);
}

#[test]
fn an_edit_that_leaves_nothing_exports_nothing() {
    assert!(
        cue_wav(
            &load("ladder-dc.wav"),
            &render((0.0, None), &[[0.0, 10.0]], (0.0, 0.0), false, 1.0)
        )
        .is_none()
    );
}
