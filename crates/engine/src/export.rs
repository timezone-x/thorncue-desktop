//! Rendering a cue to a file (FORMAT.md section 7): the audio as GO plays it, with trims,
//! cuts, volume and fades applied (one pass if it loops), master volume ignored, clipped to
//! full scale. `cue_samples` is the one render; `cue_wav` and `cue_mp3` encode it.
//!
//! It uses playback's own edit (`edit::apply`, 4 ms joins) and fade envelope
//! (`gain::envelope`), so the file is what GO plays. Never called on the GO path or the audio
//! thread: the app runs it on a blocking task.

use kira::sound::static_sound::StaticSoundData;

use crate::edit::{EditKey, apply};
use crate::gain::{FadeShape, envelope, fade_out_start};

/// What a cue sounds like, for rendering. Volume is linear amplitude (1.0 = as recorded).
#[derive(Debug, Clone, PartialEq)]
pub struct CueRender {
    pub shape: FadeShape,
    pub volume: f64,
}

/// A rendered cue: interleaved samples in −1..=1 (already clipped), at the file's own rate.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub sample_rate: u32,
    /// 1 when the decoded audio is mono (both channels equal in every frame), otherwise 2.
    pub channels: u16,
    pub samples: Vec<f32>,
}

impl Rendered {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    /// Edited length in seconds (FORMAT.md 7.4 `length`).
    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / self.sample_rate as f64
    }
}

/// Cue `r` rendered from `raw` (the decoded file), or `None` when the edit leaves nothing.
pub fn cue_samples(raw: &StaticSoundData, r: &CueRender) -> Option<Rendered> {
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
    let mut samples = Vec::with_capacity(frames.len() * channels as usize);
    for (i, f) in frames.iter().enumerate() {
        let t = i as f64 / sr as f64;
        let g = volume * envelope(t, s.fade_in, fs, length) as f32;
        samples.push((f.left * g).clamp(-1.0, 1.0));
        if !mono {
            samples.push((f.right * g).clamp(-1.0, 1.0));
        }
    }
    Some(Rendered {
        sample_rate: sr,
        channels,
        samples,
    })
}

/// A 16-bit PCM WAV of cue `r`, or `None` when the edit leaves nothing.
pub fn cue_wav(raw: &StaticSoundData, r: &CueRender) -> Option<Vec<u8>> {
    cue_samples(raw, r).map(|x| encode_wav(&x))
}

/// FORMAT.md 7.2 WAV: the 44-byte canonical header, then 16-bit little-endian PCM.
pub fn encode_wav(r: &Rendered) -> Vec<u8> {
    let channels = r.channels;
    let sr = r.sample_rate;
    let data_bytes = r.samples.len() * 2;
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
    for &v in &r.samples {
        out.extend_from_slice(&pcm16(v).to_le_bytes());
    }
    out
}

/// `round(v × 32768)` if negative, `round(v × 32767)` otherwise (round half away from zero).
fn pcm16(v: f32) -> i16 {
    let v = v.clamp(-1.0, 1.0);
    if v < 0.0 {
        (v * 32768.0).round() as i16
    } else {
        (v * 32767.0).round() as i16
    }
}

/// The MP3 a cue at `sample_rate` gets (FORMAT.md 7.2): (output rate, kbps). MP3 allows nine
/// rates; any other is resampled (to 44.1 kHz for multiples of 11,025 Hz, else 48 kHz). Lower
/// rates have lower bit-rate ceilings, so the highest each allows is used.
pub fn mp3_settings(sample_rate: u32) -> (u32, u16) {
    let out = match sample_rate {
        8000 | 11025 | 12000 | 16000 | 22050 | 24000 | 32000 | 44100 | 48000 => sample_rate,
        r if r % 11025 == 0 => 44100,
        _ => 48000,
    };
    let kbps = match out {
        8000..=12000 => 64,
        16000..=24000 => 160,
        _ => 320,
    };
    (out, kbps)
}

/// An MP3 of cue `r` (FORMAT.md 7.2), or `Ok(None)` when the edit leaves nothing. The error is
/// LAME's, for the log.
pub fn cue_mp3(raw: &StaticSoundData, r: &CueRender) -> Result<Option<Vec<u8>>, String> {
    match cue_samples(raw, r) {
        Some(x) => encode_mp3(&x).map(Some),
        None => Ok(None),
    }
}

/// FORMAT.md 7.2 MP3: LAME, CBR at `mp3_settings`, joint stereo, with the Xing/LAME info
/// frame (so players skip the encoder delay and the length round-trips).
pub fn encode_mp3(r: &Rendered) -> Result<Vec<u8>, String> {
    use mp3lame_encoder::{Bitrate, Builder, FlushGap, InterleavedPcm, Mode, MonoPcm, Quality};
    use std::num::NonZeroU32;

    let (out_rate, kbps) = mp3_settings(r.sample_rate);
    let e = |what: &str, err: &dyn std::fmt::Display| format!("LAME {what}: {err}");
    let mut b = Builder::new().ok_or("LAME couldn't start")?;
    b.set_sample_rate(r.sample_rate)
        .map_err(|x| e("sample rate", &x))?;
    if out_rate != r.sample_rate {
        b.set_output_sample_rate(NonZeroU32::new(out_rate))
            .map_err(|x| e("output rate", &x))?;
    }
    b.set_num_channels(r.channels as u8)
        .map_err(|x| e("channels", &x))?;
    b.set_brate(match kbps {
        64 => Bitrate::Kbps64,
        160 => Bitrate::Kbps160,
        _ => Bitrate::Kbps320,
    })
    .map_err(|x| e("bit rate", &x))?;
    b.set_mode(if r.channels == 1 {
        Mode::Mono
    } else {
        Mode::JointStereo
    })
    .map_err(|x| e("mode", &x))?;
    b.set_quality(Quality::Best).map_err(|x| e("quality", &x))?;
    b.set_to_write_vbr_tag(true)
        .map_err(|x| e("info frame", &x))?;
    let mut enc = b.build().map_err(|x| e("setup", &x))?;

    let mut out = Vec::new();
    // In chunks, so the scratch buffer stays small for long cues.
    let chunk_frames = 1 << 16;
    for chunk in r.samples.chunks(chunk_frames * r.channels as usize) {
        out.reserve(mp3lame_encoder::max_required_buffer_size(
            chunk.len() / r.channels as usize,
        ));
        let res = if r.channels == 1 {
            enc.encode_to_vec(MonoPcm(chunk), &mut out)
        } else {
            enc.encode_to_vec(InterleavedPcm(chunk), &mut out)
        };
        res.map_err(|x| e("encode", &x))?;
    }
    out.reserve(7200);
    enc.flush_to_vec::<FlushGap>(&mut out)
        .map_err(|x| e("flush", &x))?;
    // The info frame was written as a placeholder at the start; fill it in now.
    let mut tag = Vec::with_capacity(enc.lame_tag_size());
    if let Some(n) = enc.lame_tag_encode_to_vec(&mut tag) {
        let n = n.get();
        if n <= out.len() {
            out[..n].copy_from_slice(&tag[..n]);
        }
    }
    Ok(out)
}
