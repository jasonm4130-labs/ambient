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

/// Silero reports 0.6–0.9 on quiet room noise — confidently enough that no
/// probability threshold separates it from real speech at 0.92–1.00. Level
/// does separate a turn's quiet EDGES from its speech, and trimming them
/// matters because a noise prefix does not merely add junk, it derails the
/// whole decode. Measured on one utterance: "The migration is scheduled for
/// Thursday morning" trimmed at decoder confidence 0.992, against "The gap is
/// not closed. If anything, yeah, why don't you..." at 0.613 with 2.6 s of room
/// noise in front of it.
///
/// The floor is RELATIVE to the track's own loudest content, and only that.
/// An absolute floor was tried and removed: it threw away a real recording
/// whole. Quiet speech captured across a room measured −42 dB p90 while a
/// genuinely silent room measured −44 dB — **2 dB apart** — so any absolute
/// threshold that rejects the empty room also rejects real speech. Level
/// cannot tell them apart, Silero cannot, and neither can decoder confidence
/// (0.959 on the real speech, 0.917 on a hallucination invented from silence).
///
/// So a room track that is nothing but noise still reaches the recogniser and
/// can still produce an invented line. That is a semantic problem and belongs
/// to a downstream cleanup pass that can read the words, not to a threshold
/// here. `asr::Recognizer::last_confidence` is recorded per line to give that
/// pass something to weigh.
const RELATIVE_FLOOR_DB: f32 = 15.0;
/// Digital silence only — a guard against dividing attention by zero, not a
/// judgement about what is speech.
const ABSOLUTE_FLOOR_DB: f32 = -70.0;

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

    /// Per-frame RMS, on the same frame grid as `probabilities`.
    fn frame_rms(samples: &[f32]) -> Vec<f32> {
        (0..samples.len())
            .step_by(FRAME)
            .map(|start| {
                let f = &samples[start..(start + FRAME).min(samples.len())];
                (f.iter().map(|s| s * s).sum::<f32>() / f.len().max(1) as f32).sqrt()
            })
            .collect()
    }

    /// The level below which audio is treated as room noise rather than speech.
    fn speech_floor(rms: &[f32]) -> f32 {
        let mut sorted: Vec<f32> = rms.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p90 = sorted
            .get(sorted.len().saturating_mul(9) / 10)
            .copied()
            .unwrap_or(0.0);
        let relative = p90 * 10f32.powf(-RELATIVE_FLOOR_DB / 20.0);
        let absolute = 10f32.powf(ABSOLUTE_FLOOR_DB / 20.0);
        relative.max(absolute)
    }

    /// Pull each turn's start and end in to the first and last frame that
    /// clears `floor`, and drop a turn with nothing left. The `PAD_MS` margin
    /// is re-applied afterwards so consonants survive, but never back out
    /// beyond where the turn already started.
    fn trim_quiet(segs: Vec<Segment>, rms: &[f32], floor: f32) -> Vec<Segment> {
        let pad = PAD_MS * SR / 1000;
        let min_speech = MIN_SPEECH_MS * SR / 1000;
        let loud = |i: usize| rms.get(i).is_some_and(|&r| r >= floor);

        segs.into_iter()
            .filter_map(|s| {
                let (f0, f1) = (s.start / FRAME, s.end.div_ceil(FRAME));
                let first = (f0..f1).find(|&i| loud(i))?;
                let last = (f0..f1).rev().find(|&i| loud(i))?;
                let start = (first * FRAME).saturating_sub(pad).max(s.start);
                let end = ((last + 1) * FRAME + pad).min(s.end);
                (end.saturating_sub(start) >= min_speech).then_some(Segment { start, end })
            })
            .collect()
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
        Ok(self.detect(samples)?.1)
    }

    /// The per-frame probabilities and the speech regions they imply, with
    /// quiet edges trimmed off. Both callers need the probabilities, so they
    /// are computed once and handed back.
    fn detect(&mut self, samples: &[f32]) -> Result<(Vec<f32>, Vec<Segment>)> {
        let probs = self.probabilities(samples)?;
        let rms = Self::frame_rms(samples);
        let floor = Self::speech_floor(&rms);
        let segs = Self::trim_quiet(Self::segments_from(&probs, samples.len()), &rms, floor);
        Ok((probs, segs))
    }

    /// Hysteresis over the per-frame probabilities. No model and no audio,
    /// so it is testable on its own.
    fn segments_from(probs: &[f32], n_samples: usize) -> Vec<Segment> {
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
            // NaN and the infinities compare false against both thresholds, so
            // an unguarded one holds a segment open to the end of the
            // recording. Read it as silence and let the hysteresis decide.
            let p = if p.is_finite() { p } else { 0.0 };
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

    /// One segment per turn: speech regions as detected, with anything longer
    /// than `max_seconds` split at the least-voiced frame near the boundary —
    /// the best available approximation of a pause. Nothing is merged, so a
    /// silence between two people stays a boundary and each record can carry
    /// its own speaker.
    pub fn turns(&mut self, samples: &[f32], max_seconds: usize) -> Result<Vec<Segment>> {
        let max = max_seconds * SR;
        let (probs, segs) = self.detect(samples)?;

        let mut split: Vec<Segment> = Vec::new();
        for s in segs {
            let mut start = s.start;
            while s.end - start > max {
                // Look for the quietest frame in the last quarter of the window.
                let target = start + max;
                let lo = (target - max / 4).max(start + SR);
                let (lo_f, hi_f) = (lo / FRAME, (target / FRAME).min(probs.len()));
                let cut = probs
                    .get(lo_f..hi_f)
                    .and_then(crate::features::argmin)
                    .map(|f| (lo_f + f) * FRAME)
                    .unwrap_or(target);
                split.push(Segment { start, end: cut });
                start = cut;
            }
            if s.end > start {
                split.push(Segment { start, end: s.end });
            }
        }

        Ok(split)
    }

    /// `turns`, with neighbours merged back together wherever they still fit
    /// inside `max_seconds`. Fewer, larger recogniser calls — right when the
    /// output is one block of text, wrong when each record needs its own
    /// speaker, which is why `record` uses `turns` instead.
    pub fn chunks(&mut self, samples: &[f32], max_seconds: usize) -> Result<Vec<Segment>> {
        let max = max_seconds * SR;
        let mut out: Vec<Segment> = Vec::new();
        for s in self.turns(samples, max_seconds)? {
            match out.last_mut() {
                Some(cur) if s.end - cur.start <= max => cur.end = s.end,
                _ => out.push(s),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frame levels in dB, laid out on the `FRAME` grid.
    fn track(db: &[f32]) -> Vec<f32> {
        db.iter().map(|d| 10f32.powf(d / 20.0)).collect()
    }

    #[test]
    fn a_turn_loses_its_quiet_lead_in() {
        // Room noise at −45 dB in front of speech at −27, which is the shape
        // that made Parakeet invent a sentence.
        let mut db = vec![-45.0; 80];
        db.extend(std::iter::repeat_n(-27.0, 120));
        let rms = track(&db);
        let floor = Vad::speech_floor(&rms);
        let segs = Vad::trim_quiet(
            vec![Segment {
                start: 0,
                end: 200 * FRAME,
            }],
            &rms,
            floor,
        );
        assert_eq!(segs.len(), 1);
        // Starts inside the speech, allowing for the re-applied PAD_MS margin.
        assert!(segs[0].start > 70 * FRAME, "start was {}", segs[0].start);
        assert_eq!(segs[0].end, 200 * FRAME);
    }

    #[test]
    fn speech_throughout_is_left_alone() {
        let rms = track(&vec![-25.0; 200]);
        let floor = Vad::speech_floor(&rms);
        let segs = Vad::trim_quiet(
            vec![Segment {
                start: 0,
                end: 200 * FRAME,
            }],
            &rms,
            floor,
        );
        assert_eq!(segs.len(), 1);
        assert_eq!((segs[0].start, segs[0].end), (0, 200 * FRAME));
    }

    #[test]
    fn a_quiet_recording_is_judged_against_itself() {
        // Everything 20 dB down. This is the case an absolute floor destroyed:
        // a real recording whose speech sat at −47 dB came back empty. The
        // relative floor finds the speech wherever the track happens to sit.
        let mut db = vec![-70.0; 80];
        db.extend(std::iter::repeat_n(-47.0, 120));
        let rms = track(&db);
        let floor = Vad::speech_floor(&rms);
        let segs = Vad::trim_quiet(
            vec![Segment {
                start: 0,
                end: 200 * FRAME,
            }],
            &rms,
            floor,
        );
        assert_eq!(segs.len(), 1, "the speech was thrown away");
        assert!(segs[0].start > 70 * FRAME, "start was {}", segs[0].start);
    }

    #[test]
    fn a_noise_only_track_is_no_longer_rejected_here() {
        // Deliberate, and the honest cost of the test above: level cannot tell
        // quiet speech from room noise — measured 2 dB apart — so this passes
        // through to be judged on its words downstream rather than dropped on a
        // threshold that would also drop real speech.
        let rms = track(&vec![-45.0; 200]);
        let floor = Vad::speech_floor(&rms);
        let segs = Vad::trim_quiet(
            vec![Segment {
                start: 0,
                end: 200 * FRAME,
            }],
            &rms,
            floor,
        );
        assert_eq!(segs.len(), 1);
    }
}

/// The hysteresis is a pure function of the per-frame probabilities, so it is
/// exercised without a model: `cargo test vad::segments`. Its own module so
/// that filter selects exactly these.
#[cfg(test)]
mod segments {
    use super::*;

    const PAD: usize = PAD_MS * SR / 1000;

    /// Frames on the production grid: each `(value, ms)` contributes as many
    /// frames as that much audio would, rounding up the way `probabilities`
    /// does when it zero-pads the tail frame.
    fn probs(pattern: &[(f32, u32)]) -> Vec<f32> {
        pattern
            .iter()
            .flat_map(|&(v, ms)| std::iter::repeat_n(v, (ms as usize * SR / 1000).div_ceil(FRAME)))
            .collect()
    }

    /// Samples a probability array implies.
    fn samples(p: &[f32]) -> usize {
        p.len() * FRAME
    }

    fn run(p: &[f32]) -> Vec<Segment> {
        Vad::segments_from(p, samples(p))
    }

    #[test]
    fn silence_yields_nothing() {
        assert!(run(&probs(&[(0.0, 2000)])).is_empty());
    }

    #[test]
    fn a_turn_is_padded_either_side() {
        let p = probs(&[(0.0, 1000), (1.0, 1000), (0.0, 1000)]);
        let lead = samples(&probs(&[(0.0, 1000)]));
        let speech_end = lead + samples(&probs(&[(1.0, 1000)]));
        let segs = run(&p);
        assert_eq!(segs.len(), 1, "{segs:?}");
        assert!(
            segs[0].start.abs_diff(lead - PAD) < FRAME,
            "start {} is not PAD_MS before {lead}",
            segs[0].start
        );
        assert!(
            segs[0].end.abs_diff(speech_end + PAD) < FRAME,
            "end {} is not PAD_MS after {speech_end}",
            segs[0].end
        );
    }

    #[test]
    fn a_turn_shorter_than_min_speech_is_dropped() {
        assert!(run(&probs(&[(0.0, 500), (1.0, 100), (0.0, 1000)])).is_empty());
    }

    #[test]
    fn a_gap_under_min_silence_does_not_split() {
        let segs = run(&probs(&[(1.0, 500), (0.0, 200), (1.0, 500)]));
        assert_eq!(segs.len(), 1, "{segs:?}");
    }

    #[test]
    fn a_gap_over_min_silence_splits() {
        let segs = run(&probs(&[(1.0, 500), (0.0, 600), (1.0, 500)]));
        assert_eq!(segs.len(), 2, "{segs:?}");
    }

    #[test]
    fn a_gap_of_exactly_min_silence_splits() {
        // The rule, as the code has it: MIN_SILENCE_MS is inclusive, and the
        // silence run is counted in whole frames, so 400 ms of gap becomes 13
        // frames (6656 samples) against a 6400-sample threshold and closes the
        // segment. A gap at the boundary splits.
        let segs = run(&probs(&[(1.0, 500), (0.0, 400), (1.0, 500)]));
        assert_eq!(segs.len(), 2, "{segs:?}");
    }

    #[test]
    fn probabilities_below_on_never_open_a_turn() {
        assert!(run(&probs(&[(0.45, 1000)])).is_empty());
    }

    #[test]
    fn a_turn_continues_through_probabilities_above_off() {
        let p = probs(&[(1.0, 500), (0.40, 1000)]);
        let segs = run(&p);
        assert_eq!(segs.len(), 1, "{segs:?}");
        assert_eq!(segs[0].end, samples(&p), "the 0.40 run is still the turn");
    }

    #[test]
    fn a_turn_running_to_the_end_stops_at_the_last_sample() {
        let p = probs(&[(0.0, 500), (1.0, 1000)]);
        let segs = run(&p);
        assert_eq!(segs.len(), 1, "{segs:?}");
        assert_eq!(segs[0].end, samples(&p));

        // And never past the recording when it does not fill its last frame.
        let ragged = samples(&p) - 300;
        let segs = Vad::segments_from(&p, ragged);
        assert_eq!(segs.len(), 1, "{segs:?}");
        assert_eq!(segs[0].end, ragged);
    }

    #[test]
    fn a_non_finite_probability_reads_as_silence() {
        // NaN compares false against both thresholds, so left alone it neither
        // opens nor closes a turn — it holds one open to the end of the
        // recording, swallowing the silence after it.
        let p = probs(&[(1.0, 500), (f32::NAN, 1000), (0.0, 1000)]);
        let speech_end = samples(&probs(&[(1.0, 500)]));
        let segs = run(&p);
        assert_eq!(segs.len(), 1, "{segs:?}");
        assert_eq!(segs[0].end, speech_end + PAD);
    }
}
