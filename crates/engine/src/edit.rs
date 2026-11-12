//! Edited audio (FORMAT.md 2.5): the source from `trim_start` to `trim_end`, minus `cuts`,
//! joined with 4 ms linear ramps. Ported from reference/web/app.js `keptSegments`,
//! `subtractCuts` and `renderSegments` (L130-202).
//!
//! Trims alone use `StaticSoundData::slice` (no copy). Cuts render a new buffer. Results are
//! cached by `EditKey`, so cues sharing an asset and an edit share one buffer.

use std::sync::Arc;

use kira::Frame;
use kira::sound::static_sound::StaticSoundData;

/// Identifies one edit of one file. Floats are compared by bit pattern: the manifest
/// normalises them (rounded to 4 places), so equal edits have equal bits.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EditKey {
    trim_start: u64,
    trim_end: Option<u64>,
    cuts: Vec<(u64, u64)>,
}

impl EditKey {
    pub fn new(trim_start: f64, trim_end: Option<f64>, cuts: &[[f64; 2]]) -> Self {
        Self {
            trim_start: trim_start.to_bits(),
            trim_end: trim_end.map(f64::to_bits),
            cuts: cuts
                .iter()
                .map(|c| (c[0].to_bits(), c[1].to_bits()))
                .collect(),
        }
    }

    fn trim_start(&self) -> f64 {
        f64::from_bits(self.trim_start)
    }

    fn trim_end(&self) -> Option<f64> {
        self.trim_end.map(f64::from_bits)
    }

    fn cuts(&self) -> Vec<[f64; 2]> {
        self.cuts
            .iter()
            .map(|&(a, b)| [f64::from_bits(a), f64::from_bits(b)])
            .collect()
    }

    pub fn is_unedited(&self) -> bool {
        self.trim_start() <= 0.0 && self.trim_end().is_none() && self.cuts.is_empty()
    }
}

/// The parts of the source that play, in source seconds (web `keptSegments`).
pub fn kept_segments(
    trim_start: f64,
    trim_end: Option<f64>,
    cuts: &[[f64; 2]],
    duration: f64,
) -> Vec<(f64, f64)> {
    let start = trim_start.clamp(0.0, duration);
    let mut end = trim_end.map(|e| e.clamp(0.0, duration)).unwrap_or(duration);
    if end <= start {
        end = duration;
    }
    let mut out = vec![(start, end)];
    for c in cuts {
        let (a, b) = (c[0], c[1]);
        let mut next = Vec::with_capacity(out.len() + 1);
        for &(s, e) in &out {
            if b <= s || a >= e {
                next.push((s, e));
                continue;
            }
            if a > s {
                next.push((s, a));
            }
            if b < e {
                next.push((b, e));
            }
        }
        out = next;
    }
    out.retain(|(s, e)| e - s > 0.001);
    out
}

/// The edited audio for `key`, or `None` when the edit leaves nothing (length 0).
pub fn apply(source: &StaticSoundData, key: &EditKey) -> Option<StaticSoundData> {
    if key.is_unedited() {
        return Some(source.clone());
    }
    let sr = source.sample_rate as f64;
    let duration = source.frames.len() as f64 / sr;
    let segs = kept_segments(key.trim_start(), key.trim_end(), &key.cuts(), duration);
    match segs.as_slice() {
        [] => None,
        // Trims only: a slice of the shared buffer.
        [(s, e)] => Some(source.slice(*s..*e)),
        _ => Some(render_segments(source, &segs)),
    }
}

/// Copy the kept segments into one buffer with a 4 ms ramp at each join (web `renderSegments`).
fn render_segments(source: &StaticSoundData, segs: &[(f64, f64)]) -> StaticSoundData {
    let sr = source.sample_rate as f64;
    let from = &source.frames;
    let lens: Vec<usize> = segs
        .iter()
        .map(|&(s, e)| ((e * sr).round() as usize).saturating_sub((s * sr).round() as usize))
        .collect();
    let total: usize = lens.iter().sum::<usize>().max(1);
    let mut to = vec![Frame::ZERO; total];
    let ramp = (sr * 0.004).round() as usize;
    let mut pos = 0;
    for (i, &(s, _)) in segs.iter().enumerate() {
        let start = (s * sr).round() as usize;
        let len = lens[i].min(from.len().saturating_sub(start));
        to[pos..pos + len].copy_from_slice(&from[start..start + len]);
        let r = ramp.min(len >> 1);
        if i > 0 {
            for k in 0..r {
                to[pos + k] *= k as f32 / r as f32;
            }
        }
        if i < segs.len() - 1 {
            for k in 0..r {
                to[pos + len - 1 - k] *= k as f32 / r as f32;
            }
        }
        pos += len;
    }
    to.truncate(pos.max(1));
    StaticSoundData {
        sample_rate: source.sample_rate,
        frames: Arc::from(to),
        settings: source.settings,
        slice: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_follow_the_web() {
        assert_eq!(
            kept_segments(2.0, Some(9.0), &[[4.0, 6.0]], 10.0),
            vec![(2.0, 4.0), (6.0, 9.0)]
        );
        // Fixture 02 q01: trim 1-8, cuts 3-4 and 5.5-6: 5.5 s.
        let s = kept_segments(1.0, Some(8.0), &[[3.0, 4.0], [5.5, 6.0]], 10.0);
        assert_eq!(s, vec![(1.0, 3.0), (4.0, 5.5), (6.0, 8.0)]);
        assert!((s.iter().map(|(a, b)| b - a).sum::<f64>() - 5.5).abs() < 1e-9);
        // Cuts outside the trim do nothing; trim past the end clamps (and plays nothing).
        assert_eq!(
            kept_segments(1.0, Some(2.0), &[[5.0, 6.0]], 10.0),
            vec![(1.0, 2.0)]
        );
        assert!(kept_segments(12.0, None, &[], 10.0).is_empty());
        // Cuts that remove everything.
        assert!(kept_segments(0.0, None, &[[0.0, 10.0]], 10.0).is_empty());
    }
}
