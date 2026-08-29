//! Silero voice activity detection.
//!
//! Two jobs here. It skips silence so a two-hour recording only costs what its
//! speech costs, and — more usefully — it gives the transcriber somewhere
//! honest to cut. Fixed-length chunks slice through words and duplicate them at
//! the seam; a boundary chosen inside real silence does not.

use anyhow::Result;
use ort::session::Session;
use ort::value::Tensor;

/// The 16 kHz Silero branch consumes exactly this many samples per step.
const FRAME: usize = 512;
const SR: usize = 16_000;

/// Hysteresis: it takes a confident frame to open a segment and a clearly
/// unvoiced one to close it, so a brief dip mid-word does not split a turn.
const ON: f32 = 0.50;
const OFF: f32 = 0.35;

const MIN_SPEECH_MS: usize = 250;
const MIN_SILENCE_MS: usize = 400;
/// Keep a little audio either side; consonants at a boundary are quiet.
const PAD_MS: usize = 200;

trait OrtExt<T> {
    fn a(self) -> Result<T>;
}
impl<T> OrtExt<T> for ort::Result<T> {
    fn a(self) -> Result<T> {
        self.map_err(|e| anyhow::anyhow!(e.to_string()))
    }
}

pub struct Vad {
    session: Session,
}

#[derive(Debug, Clone, Copy)]
pub struct Segment {
    pub start: usize,
    pub end: usize,
}

impl Segment {
    pub fn seconds(&self) -> f64 {
        (self.end - self.start) as f64 / SR as f64
    }
}

impl Vad {
    pub fn load(path: &str) -> Result<Self> {
        Ok(Self {
            session: Session::builder().a()?.commit_from_file(path).a()?,
        })
    }

    /// Per-frame speech probability.
    pub fn probabilities(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        let mut h = vec![0.0f32; 2 * 64];
        let mut c = vec![0.0f32; 2 * 64];
        let mut probs = Vec::with_capacity(samples.len() / FRAME + 1);

        let mut frame = vec![0.0f32; FRAME];
        for start in (0..samples.len()).step_by(FRAME) {
            let end = (start + FRAME).min(samples.len());
            let n = end - start;
            frame[..n].copy_from_slice(&samples[start..end]);
            frame[n..].fill(0.0); // zero-pad the tail frame

            let x = Tensor::from_array((vec![1_i64, FRAME as i64], frame.clone())).a()?;
            let ht = Tensor::from_array((vec![2_i64, 1, 64], h.clone())).a()?;
            let ct = Tensor::from_array((vec![2_i64, 1, 64], c.clone())).a()?;

            let out = self
                .session
                .run(ort::inputs!["x" => &x, "h" => &ht, "c" => &ct])
                .a()?;
            let (_, p) = out["prob"].try_extract_tensor::<f32>().a()?;
            let (_, nh) = out["new_h"].try_extract_tensor::<f32>().a()?;
            let (_, nc) = out["new_c"].try_extract_tensor::<f32>().a()?;
            probs.push(p[0]);
            h = nh.to_vec();
            c = nc.to_vec();
        }
        Ok(probs)
    }

    /// Speech regions, in samples.
    pub fn segments(&mut self, samples: &[f32]) -> Result<Vec<Segment>> {
        let probs = self.probabilities(samples)?;
        Ok(self.segments_from(&probs, samples.len()))
    }

    fn segments_from(&self, probs: &[f32], n_samples: usize) -> Vec<Segment> {
        let samples_len = n_samples;
        let ms = |n: usize| n * SR / 1000;
        let min_speech = ms(MIN_SPEECH_MS);
        let min_silence = ms(MIN_SILENCE_MS);
        let pad = ms(PAD_MS);

        let mut out: Vec<Segment> = Vec::new();
        let mut in_speech = false;
        let mut start = 0usize;
        let mut silence_run = 0usize;

        for (i, &p) in probs.iter().enumerate() {
            let at = i * FRAME;
            if !in_speech {
                if p >= ON {
                    in_speech = true;
                    start = at;
                    silence_run = 0;
                }
            } else if p < OFF {
                silence_run += FRAME;
                if silence_run >= min_silence {
                    let end = at + FRAME - silence_run;
                    if end > start && end - start >= min_speech {
                        out.push(Segment { start, end });
                    }
                    in_speech = false;
                }
            } else {
                silence_run = 0;
            }
        }
        if in_speech {
            let end = samples_len;
            if end.saturating_sub(start) >= min_speech {
                out.push(Segment { start, end });
            }
        }

        // Pad, then merge anything that now overlaps.
        let mut padded: Vec<Segment> = Vec::new();
        for s in out {
            let seg = Segment {
                start: s.start.saturating_sub(pad),
                end: (s.end + pad).min(samples_len),
            };
            match padded.last_mut() {
                Some(prev) if seg.start <= prev.end => prev.end = seg.end.max(prev.end),
                _ => padded.push(seg),
            }
        }
        padded
    }

    /// Group speech into transcription-sized chunks, never cutting inside a
    /// segment unless the segment is itself longer than `max_seconds` — in
    /// which case it is split at the least-voiced frame near the boundary,
    /// which is the best available approximation of a pause.
    pub fn chunks(&mut self, samples: &[f32], max_seconds: usize) -> Result<Vec<Segment>> {
        let max = max_seconds * SR;
        let probs = self.probabilities(samples)?;
        let segs = self.segments_from(&probs, samples.len());

        let mut split: Vec<Segment> = Vec::new();
        for s in segs {
            let mut start = s.start;
            while s.end - start > max {
                // Look for the quietest frame in the last quarter of the window.
                let target = start + max;
                let lo = (target - max / 4).max(start + SR);
                let cut = (lo / FRAME..(target / FRAME).min(probs.len()))
                    .min_by(|&a, &b| probs[a].partial_cmp(&probs[b]).unwrap())
                    .map(|f| f * FRAME)
                    .unwrap_or(target);
                split.push(Segment { start, end: cut });
                start = cut;
            }
            if s.end > start {
                split.push(Segment { start, end: s.end });
            }
        }

        // Merge neighbours that still fit, so we make few large calls.
        let mut out: Vec<Segment> = Vec::new();
        for s in split {
            match out.last_mut() {
                Some(cur) if s.end - cur.start <= max => cur.end = s.end,
                _ => out.push(s),
            }
        }
        Ok(out)
    }
}
