//! Kaldi-compatible 80-bin log-mel filterbank, for the WeSpeaker embedder.
//!
//! Deliberately separate from `features.rs`. That one feeds NeMo and is
//! Slaney-scale, 128-bin, centre-padded and per-feature normalised; this one
//! feeds WeSpeaker and is HTK-scale, 80-bin, Povey-windowed and snipped at the
//! edges. Sharing a "configurable" front-end between them would mean one set of
//! constants quietly wrong for one caller.
//!
//! The stakes are higher here than for the recogniser. A near-miss filterbank
//! feeding an ASR model produces visible nonsense; feeding a speaker embedder it
//! produces embeddings that still cluster, only worse — so the damage arrives as
//! confidently mislabelled speakers. `bin/embtest.rs` exists to catch that
//! before anything is built on top.

use rustfft::{num_complex::Complex32, FftPlanner};

pub const SAMPLE_RATE: f32 = 16_000.0;
pub const N_MELS: usize = 80;
const FRAME_LENGTH: usize = 400; // 25 ms
const FRAME_SHIFT: usize = 160; // 10 ms
const N_FFT: usize = 512; // next power of two above 400
const PREEMPH: f32 = 0.97;
const LOW_FREQ: f32 = 20.0;
/// WeSpeaker expects samples at int16 scale, per the model's
/// `normalize_samples = 0` metadata — not the [-1, 1] the rest of the pipeline
/// carries.
const SAMPLE_SCALE: f32 = 32768.0;

/// HTK mel scale — Kaldi's, and not the Slaney variant in `features.rs`.
fn hz_to_mel(f: f32) -> f32 {
    1127.0 * (1.0 + f / 700.0).ln()
}

/// Only the round-trip test needs the inverse; it is here to prove the
/// forward scale is the one Kaldi uses.
#[allow(dead_code)]
fn mel_to_hz(m: f32) -> f32 {
    700.0 * ((m / 1127.0).exp() - 1.0)
}

/// Triangular filters in the mel domain, no area normalisation — Kaldi leaves
/// them peak-normalised, where librosa/Slaney would scale by bandwidth.
fn mel_filterbank() -> Vec<Vec<f32>> {
    let nyquist = SAMPLE_RATE / 2.0;
    let mel_low = hz_to_mel(LOW_FREQ);
    let mel_high = hz_to_mel(nyquist);
    let delta = (mel_high - mel_low) / (N_MELS + 1) as f32;
    let bins = N_FFT / 2 + 1;

    (0..N_MELS)
        .map(|i| {
            let left = mel_low + i as f32 * delta;
            let center = left + delta;
            let right = left + 2.0 * delta;
            (0..bins)
                .map(|b| {
                    let mel = hz_to_mel(b as f32 * SAMPLE_RATE / N_FFT as f32);
                    if mel <= left || mel >= right {
                        0.0
                    } else if mel <= center {
                        (mel - left) / (center - left)
                    } else {
                        (right - mel) / (right - center)
                    }
                })
                .collect()
        })
        .collect()
}

/// Povey window: Hann raised to 0.85. Kaldi's default, and not interchangeable
/// with the plain Hann used for the recogniser.
fn povey_window() -> Vec<f32> {
    (0..FRAME_LENGTH)
        .map(|i| {
            let hann = 0.5
                - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FRAME_LENGTH - 1) as f32).cos();
            hann.powf(0.85)
        })
        .collect()
}

/// 80-bin log-mel features, frame-major: `[frames * N_MELS]`.
///
/// Mean-normalised across time, which is what WeSpeaker's training pipeline
/// does and what makes embeddings comparable between recordings.
pub fn fbank(samples: &[f32]) -> (Vec<f32>, usize) {
    if samples.len() < FRAME_LENGTH {
        return (Vec::new(), 0);
    }

    // snip_edges: no padding, so the tail that cannot fill a frame is dropped.
    let frames = (samples.len() - FRAME_LENGTH) / FRAME_SHIFT + 1;
    let window = povey_window();
    let bank = mel_filterbank();
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N_FFT);

    let mut out = vec![0.0f32; frames * N_MELS];
    let mut buf = vec![Complex32::new(0.0, 0.0); N_FFT];
    let mut frame = vec![0.0f32; FRAME_LENGTH];

    for f in 0..frames {
        let start = f * FRAME_SHIFT;
        for (i, s) in frame.iter_mut().enumerate() {
            *s = samples[start + i] * SAMPLE_SCALE;
        }

        // Kaldi removes the DC offset per frame, before pre-emphasis.
        let mean = frame.iter().sum::<f32>() / FRAME_LENGTH as f32;
        for s in frame.iter_mut() {
            *s -= mean;
        }

        // Pre-emphasis, in place and backwards so each sample still sees its
        // untouched predecessor. Kaldi replicates the first sample rather than
        // assuming a zero before it.
        for i in (1..FRAME_LENGTH).rev() {
            frame[i] -= PREEMPH * frame[i - 1];
        }
        frame[0] -= PREEMPH * frame[0];

        for (i, b) in buf.iter_mut().enumerate() {
            *b = Complex32::new(
                if i < FRAME_LENGTH {
                    frame[i] * window[i]
                } else {
                    0.0
                },
                0.0,
            );
        }
        fft.process(&mut buf);

        let power: Vec<f32> = buf[..N_FFT / 2 + 1]
            .iter()
            .map(|c| c.re * c.re + c.im * c.im)
            .collect();

        for (m, filt) in bank.iter().enumerate() {
            let e: f32 = filt.iter().zip(&power).map(|(w, p)| w * p).sum();
            out[f * N_MELS + m] = e.max(f32::EPSILON).ln();
        }
    }

    // Cepstral mean normalisation over the utterance.
    for m in 0..N_MELS {
        let mean: f32 = (0..frames).map(|f| out[f * N_MELS + m]).sum::<f32>() / frames as f32;
        for f in 0..frames {
            out[f * N_MELS + m] -= mean;
        }
    }

    (out, frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_count_snips_edges() {
        // 1 s: (16000 - 400)/160 + 1 = 98 frames, not the 100 a centre-padded
        // front-end would produce.
        let (_, frames) = fbank(&vec![0.01f32; 16_000]);
        assert_eq!(frames, 98);
    }

    #[test]
    fn too_short_yields_nothing() {
        let (v, frames) = fbank(&vec![0.01f32; 100]);
        assert!(v.is_empty());
        assert_eq!(frames, 0);
    }

    #[test]
    fn mean_normalised_per_dimension() {
        let src: Vec<f32> = (0..16_000)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16_000.0).sin() * 0.3)
            .collect();
        let (v, frames) = fbank(&src);
        for m in 0..N_MELS {
            let mean: f32 = (0..frames).map(|f| v[f * N_MELS + m]).sum::<f32>() / frames as f32;
            assert!(mean.abs() < 1e-3, "dim {m} mean {mean}");
        }
    }

    #[test]
    fn mel_scale_is_htk_not_slaney() {
        // At 1 kHz the two scales diverge: HTK gives ~999.99, Slaney gives 15.0.
        assert!((hz_to_mel(1000.0) - 999.99).abs() < 1.0);
        assert!((mel_to_hz(hz_to_mel(3000.0)) - 3000.0).abs() < 0.1);
    }
}
