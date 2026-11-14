//! Waveform peaks for the trim editor (M7), computed on the prep pool from the decoded file
//! the engine already holds. Each column covers an equal slice of the whole (unedited) file
//! and takes the minimum and maximum over *both* channels, so a cue that is louder on the
//! right draws at its real size (the web drew the left channel only: W9).

use kira::sound::static_sound::StaticSoundData;
use serde::Serialize;

/// Columns computed per file; the UI scales them to its canvas.
pub const PEAK_COLUMNS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Peaks {
    /// Length of the whole file in seconds.
    pub duration: f64,
    pub min: Vec<f32>,
    pub max: Vec<f32>,
}

pub type PeaksReply = crossbeam_channel::Sender<Result<Peaks, String>>;

pub fn compute(data: &StaticSoundData, columns: usize) -> Peaks {
    let frames = &data.frames;
    let n = frames.len();
    let columns = columns.clamp(1, n.max(1));
    let mut min = vec![0f32; columns];
    let mut max = vec![0f32; columns];
    for c in 0..columns {
        let start = c * n / columns;
        let end = ((c + 1) * n / columns).max(start + 1).min(n);
        let (mut lo, mut hi) = (0f32, 0f32);
        for f in &frames[start..end] {
            lo = lo.min(f.left).min(f.right);
            hi = hi.max(f.left).max(f.right);
        }
        min[c] = lo;
        max[c] = hi;
    }
    Peaks {
        duration: n as f64 / data.sample_rate as f64,
        min,
        max,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kira::Frame;
    use kira::sound::static_sound::StaticSoundSettings;

    use super::*;

    fn data(frames: Vec<Frame>) -> StaticSoundData {
        StaticSoundData {
            sample_rate: 100,
            frames: Arc::from(frames),
            settings: StaticSoundSettings::default(),
            slice: None,
        }
    }

    #[test]
    fn peaks_take_the_louder_channel() {
        // Quiet on the left, loud on the right (W9: the web drew only the left).
        let frames: Vec<Frame> = (0..200)
            .map(|i| {
                if i % 2 == 0 {
                    Frame::new(0.1, 0.8)
                } else {
                    Frame::new(-0.1, -0.6)
                }
            })
            .collect();
        let p = compute(&data(frames), 4);
        assert_eq!(p.duration, 2.0);
        assert_eq!(p.max, vec![0.8; 4]);
        assert_eq!(p.min, vec![-0.6; 4]);
    }

    #[test]
    fn columns_follow_the_audio_in_time() {
        // Silence then a loud second half.
        let frames: Vec<Frame> = (0..100)
            .map(|i| Frame::from_mono(if i < 50 { 0.0 } else { 0.5 }))
            .collect();
        let p = compute(&data(frames), 10);
        assert_eq!(&p.max[..5], &[0.0; 5]);
        assert_eq!(&p.max[5..], &[0.5; 5]);
        // Never more columns than frames.
        assert_eq!(compute(&data(vec![Frame::ZERO; 3]), 4096).max.len(), 3);
    }
}
