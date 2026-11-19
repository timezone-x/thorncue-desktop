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
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| i16::from_le_bytes(c))
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

// ---- FORMAT.md section 7: the spec's render fixture, and MP3 ----

fn render_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/format/fixtures/render")
}

/// Every case in fixtures/render/index.json: same header and length as the expected WAV, and
/// every sample within the stated tolerance (FORMAT.md 7.2).
#[test]
fn matches_the_spec_render_fixtures() {
    let dir = render_fixtures();
    let index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("index.json")).unwrap()).unwrap();
    let cases = index.as_array().unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let cue = &case["cue"];
        let f = |k: &str| cue[k].as_f64().unwrap();
        let r = CueRender {
            shape: FadeShape {
                trim_start: f("trim_start"),
                trim_end: cue["trim_end"].as_f64(),
                cuts: cue["cuts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| [c[0].as_f64().unwrap(), c[1].as_f64().unwrap()])
                    .collect(),
                fade_in: f("fade_in"),
                fade_out: f("fade_out"),
                looping: cue["loop"].as_bool().unwrap(),
            },
            volume: f("volume"),
        };
        let raw = StaticSoundData::from_file(dir.join(case["source"].as_str().unwrap())).unwrap();
        let ours = cue_wav(&raw, &r).expect(name);
        let expected = std::fs::read(dir.join(case["expected"].as_str().unwrap())).unwrap();
        assert_eq!(ours.len(), expected.len(), "{name}: length");
        assert_eq!(ours[..44], expected[..44], "{name}: header");
        let tol = case["tolerance"].as_i64().unwrap() as i32;
        let (a, b) = (parse(&ours), parse(&expected));
        let worst = a
            .samples
            .iter()
            .zip(&b.samples)
            .map(|(x, y)| (*x as i32 - *y as i32).abs())
            .max()
            .unwrap();
        assert!(
            worst <= tol,
            "{name}: a sample is {worst} steps off (tolerance {tol})"
        );
    }
}

#[test]
fn mp3_rates_and_bit_rates_follow_the_spec() {
    use engine::mp3_settings;
    assert_eq!(mp3_settings(44100), (44100, 320));
    assert_eq!(mp3_settings(48000), (48000, 320));
    assert_eq!(mp3_settings(32000), (32000, 320));
    assert_eq!(mp3_settings(24000), (24000, 160));
    assert_eq!(mp3_settings(22050), (22050, 160));
    assert_eq!(mp3_settings(16000), (16000, 160));
    assert_eq!(mp3_settings(12000), (12000, 64));
    assert_eq!(mp3_settings(8000), (8000, 64));
    // Not an MP3 rate: 88.2 kHz is a multiple of 11,025, 96 kHz isn't.
    assert_eq!(mp3_settings(88200), (44100, 320));
    assert_eq!(mp3_settings(96000), (48000, 320));
}

/// Decoded length (frames) and rate of an MP3, read back with kira's decoder.
fn decode_mp3(bytes: Vec<u8>) -> (usize, u32) {
    let d = StaticSoundData::from_cursor(std::io::Cursor::new(bytes)).unwrap();
    (d.frames.len(), d.sample_rate)
}

/// MPEG frame header: bit rate (kbps) and channel mode of the first audio frame after the info
/// frame. Layer III, MPEG-1 (32 kHz and up) or MPEG-2/2.5.
fn frame_header(b: &[u8], skip_first: bool) -> (u32, u8) {
    let mut i = 0;
    let mut seen = 0;
    while i + 4 <= b.len() {
        if b[i] == 0xFF && b[i + 1] & 0xE0 == 0xE0 {
            let v = (b[i + 1] >> 3) & 3; // 3 = MPEG-1, 2 = MPEG-2, 0 = MPEG-2.5
            let idx = (b[i + 2] >> 4) as usize;
            let mode = b[i + 3] >> 6;
            const V1: [u32; 15] = [
                0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
            ];
            const V2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
            let kbps = if v == 3 { V1[idx] } else { V2[idx] };
            if seen == 1 || !skip_first {
                return (kbps, mode);
            }
            seen += 1;
            // Skip this frame's header so the next search starts inside it.
            i += 4;
            while i + 1 < b.len() && !(b[i] == 0xFF && b[i + 1] & 0xE0 == 0xE0) {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    panic!("no MPEG frame found");
}

#[test]
fn mp3_has_the_info_frame_cbr_and_the_rendered_length() {
    // Ladder (8 kHz mono): 64 kbps, MPEG-2.5; the 4-6 s cut leaves 5 s.
    let r = render((2.0, Some(9.0)), &[[4.0, 6.0]], (0.5, 0.5), false, 1.0);
    let raw = load("ladder-dc.wav");
    let rendered = engine::cue_samples(&raw, &r).unwrap();
    let mp3 = engine::cue_mp3(&raw, &r).unwrap().unwrap();
    assert!(mp3.windows(4).any(|w| w == b"Info"), "CBR info frame");
    assert!(mp3.windows(4).any(|w| w == b"LAME"), "LAME tag");
    let (frames, rate) = decode_mp3(mp3.clone());
    assert_eq!(rate, 8000);
    assert!(
        frames.abs_diff(rendered.frames()) <= 576,
        "decoded {frames} frames, rendered {}",
        rendered.frames()
    );
    assert_eq!(frame_header(&mp3, true), (64, 3), "64 kbps mono");

    // Stereo 48 kHz: 320 kbps joint stereo, length within one frame (1,152).
    let raw = load("stereo-48k.wav");
    let r = render((0.0, None), &[], (0.0, 0.0), false, 1.0);
    let rendered = engine::cue_samples(&raw, &r).unwrap();
    assert_eq!(rendered.channels, 2);
    let mp3 = engine::cue_mp3(&raw, &r).unwrap().unwrap();
    let (frames, rate) = decode_mp3(mp3.clone());
    assert_eq!(rate, 48000);
    assert!(frames.abs_diff(rendered.frames()) <= 1152);
    assert_eq!(frame_header(&mp3, true), (320, 1), "320 kbps joint stereo");
}

#[test]
fn wav_and_mp3_come_from_the_same_render() {
    let raw = load("ladder-dc.wav");
    let r = render((1.0, Some(4.0)), &[], (0.5, 1.0), false, 0.8);
    let samples = engine::cue_samples(&raw, &r).unwrap();
    assert_eq!(engine::encode_wav(&samples), cue_wav(&raw, &r).unwrap());
    assert!((samples.seconds() - 3.0).abs() < 1e-9);
    assert!(
        engine::cue_mp3(
            &raw,
            &render((0.0, None), &[[0.0, 10.0]], (0.0, 0.0), false, 1.0)
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn rates_mp3_cant_store_are_resampled() {
    use engine::kira::Frame;
    use engine::kira::sound::static_sound::StaticSoundSettings;
    for (rate, want) in [(88_200u32, 44_100u32), (96_000, 48_000)] {
        let n = rate as usize * 2; // 2 s
        let frames: Vec<Frame> = (0..n)
            .map(|i| {
                let v = (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5;
                Frame::new(v, -v)
            })
            .collect();
        let raw = StaticSoundData {
            sample_rate: rate,
            frames: frames.into(),
            settings: StaticSoundSettings::default(),
            slice: None,
        };
        let mp3 = engine::cue_mp3(&raw, &render((0.0, None), &[], (0.0, 0.0), false, 1.0))
            .unwrap()
            .unwrap();
        let (got, got_rate) = decode_mp3(mp3);
        assert_eq!(got_rate, want);
        assert!(
            got.abs_diff(want as usize * 2) <= 1152,
            "{rate}: {got} frames"
        );
    }
}
