//! "Is this really audio we can play?" (FORMAT.md 4.5): probe the container and decode the
//! first packets with symphonia, the decoder the engine uses (through kira).

use std::path::Path;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Shown after "X isn't audio ThornCue can play."
pub(crate) const UNPLAYABLE_HINT: &str = "It may be damaged, or in a format ThornCue doesn't support (use MP3, WAV, OGG, FLAC, M4A or AIFF).";

/// How many packets to decode before calling a file good.
const PACKETS_TO_CHECK: usize = 8;

/// `Ok(())` if the file opens and at least one packet decodes. The error is a short
/// reason, for logs; callers turn it into a message about which file failed.
pub fn check_audio_file(path: &Path) -> std::result::Result<(), String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("not a known audio format: {e}"))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or("no audio track")?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("no audio codec parameters")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| format!("unsupported codec: {e}"))?;

    let mut decoded = 0;
    let mut tried = 0;
    while decoded < PACKETS_TO_CHECK && tried < PACKETS_TO_CHECK * 4 {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(e) => return Err(format!("can't read packets: {e}")),
        };
        if packet.track_id != track_id {
            continue;
        }
        tried += 1;
        match decoder.decode(&packet) {
            Ok(_) => decoded += 1,
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decode failed: {e}")),
        }
    }
    if decoded == 0 {
        return Err("no packet decoded".into());
    }
    Ok(())
}

/// What the container says about an audio file, without decoding it: for estimates and for
/// the cue list's length when originals are exported (FORMAT.md 7.3-7.4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub channels: u16,
    /// Seconds, when the container says (almost always; VBR MP3 without a header may not).
    pub duration: Option<f64>,
}

pub fn audio_info(path: &Path) -> std::result::Result<AudioInfo, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("not a known audio format: {e}"))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or("no audio track")?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("no audio codec parameters")?;
    let sample_rate = params.sample_rate.ok_or("no sample rate")?;
    let channels = params
        .channels
        .as_ref()
        .map(|c| c.count() as u16)
        .unwrap_or(2)
        .clamp(1, 2);
    let duration = track.num_frames.map(|n| {
        n as f64
            / track.time_base.map_or(sample_rate as f64, |tb| {
                tb.denom.get() as f64 / tb.numer.get() as f64
            })
    });
    Ok(AudioInfo {
        sample_rate,
        channels,
        duration,
    })
}
