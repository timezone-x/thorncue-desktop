//! Export one cue as a WAV file: the audio as it plays, with trims, cuts, volume and fades
//! applied (one pass if it loops). Ported from thorncue-web `static/app.js` "Export WAV"
//! (`encodeWav`), but with playback's own edit (`edit::apply`, 4 ms joins) and fade envelope
//! (`gain::envelope`), so the file is what GO plays, sample for sample.
//!
//! Never called on the GO path or the audio thread: the app runs it on a blocking task.

use kira::sound::static_sound::StaticSoundData;

use crate::edit::{EditKey, apply};
use crate::gain::{FadeShape, envelope, fade_out_start};

/// What a cue sounds like, for `cue_wav`. Volume is linear amplitude (1.0 = as recorded).
#[derive(Debug, Clone, PartialEq)]
pub struct CueRender {
    pub shape: FadeShape,
    pub volume: f64,
}

/// A 16-bit PCM WAV of `raw` (the decoded file) as cue `r` plays it, or `None` when the edit
/// leaves nothing to play. Mono when the decoded audio is mono (both channels identical),
/// otherwise stereo; the file's own sample rate.
pub fn cue_wav(raw: &StaticSoundData, r: &CueRender) -> Option<Vec<u8>> {
    let s = &r.shape;
    let edited = apply(raw, &EditKey::new(s.trim_start, s.trim_end, &s.cuts))?;
    // A trim is a slice over the whole decoded buffer: take only the sliced frames.
    let frames: &[kira::Frame] = match edited.slice {
        Some((a, b)) => &edited.frames[a.min(edited.frames.len())..b.min(edited.frames.len())],
        None => &edited.frames,
    };
    if frames.is_empty() {
        return None;
    }
    let sr = edited.sample_rate;
    let length = frames.len() as f64 / sr as f64;
    let fs = fade_out_start(s.fade_in, s.fade_out, length, s.looping);
    let volume = r.volume.clamp(0.0, 2.0) as f32;
    let mono = frames.iter().all(|f| f.left == f.right);
    let channels: u16 = if mono { 1 } else { 2 };

    let data_bytes = frames.len() * channels as usize * 2;
    let mut out = Vec::with_capacity(44 + data_bytes);
    let u32le = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    let u16le = |out: &mut Vec<u8>, v: u16| out.extend_from_slice(&v.to_le_bytes());
    out.extend_from_slice(b"RIFF");
    u32le(&mut out, (36 + data_bytes) as u32);
    out.extend_from_slice(b"WAVEfmt ");
    u32le(&mut out, 16);
    u16le(&mut out, 1); // PCM
    u16le(&mut out, channels);
    u32le(&mut out, sr);
    u32le(&mut out, sr * channels as u32 * 2);
    u16le(&mut out, channels * 2);
    u16le(&mut out, 16);
    out.extend_from_slice(b"data");
    u32le(&mut out, data_bytes as u32);

    let pcm = |v: f32| -> i16 {
        let v = v.clamp(-1.0, 1.0);
        if v < 0.0 {
            (v * 32768.0).round() as i16
        } else {
            (v * 32767.0).round() as i16
        }
    };
    for (i, f) in frames.iter().enumerate() {
        let t = i as f64 / sr as f64;
        let g = volume * envelope(t, s.fade_in, fs, length) as f32;
        out.extend_from_slice(&pcm(f.left * g).to_le_bytes());
        if !mono {
            out.extend_from_slice(&pcm(f.right * g).to_le_bytes());
        }
    }
    Some(out)
}
