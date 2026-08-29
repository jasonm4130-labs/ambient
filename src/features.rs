//! NeMo `AudioToMelSpectrogramPreprocessor`, reimplemented.
//!
//! Parakeet is sensitive to getting this exactly right — a filterbank that is
//! merely close produces fluent-looking nonsense rather than an obvious error,
//! so the constants here mirror NeMo's defaults rather than being tuned.

use rustfft::{num_complex::Complex32, FftPlanner};

pub const SAMPLE_RATE: f32 = 16_000.0;
pub const N_FFT: usize = 512;
pub const WIN_LENGTH: usize = 400; // 25 ms
pub const HOP_LENGTH: usize = 160; // 10 ms
pub const N_MELS: usize = 128;

const PREEMPH: f32 = 0.97;
const LOG_GUARD: f32 = 5.960_464_5e-8; // 2^-24, NeMo's log_zero_guard_value
const NORM_EPS: f32 = 1e-5;

/// Slaney mel scale (librosa `htk=False`), which is what NeMo uses.
fn hz_to_mel(f: f32) -> f32 {
    const F_SP: f32 = 200.0 / 3.0;
    const MIN_LOG_HZ: f32 = 1000.0;
    const MIN_LOG_MEL: f32 = MIN_LOG_HZ / F_SP;
    let logstep = (6.4f32).ln() / 27.0;
    if f >= MIN_LOG_HZ {
        MIN_LOG_MEL + (f / MIN_LOG_HZ).ln() / logstep
    } else {
        f / F_SP
    }
}

fn mel_to_hz(m: f32) -> f32 {
    const F_SP: f32 = 200.0 / 3.0;
    const MIN_LOG_HZ: f32 = 1000.0;
    const MIN_LOG_MEL: f32 = MIN_LOG_HZ / F_SP;
    let logstep = (6.4f32).ln() / 27.0;
    if m >= MIN_LOG_MEL {
        MIN_LOG_HZ * (logstep * (m - MIN_LOG_MEL)).exp()
    } else {
        F_SP * m
    }
}

/// Triangular mel filterbank with Slaney area normalisation: `[N_MELS][N_FFT/2+1]`.
fn mel_filterbank() -> Vec<Vec<f32>> {
    let n_bins = N_FFT / 2 + 1;
    let fmin = 0.0;
    let fmax = SAMPLE_RATE / 2.0;

    let mel_min = hz_to_mel(fmin);
    let mel_max = hz_to_mel(fmax);
    let points: Vec<f32> = (0..N_MELS + 2)
        .map(|i| mel_to_hz(mel_min + (mel_max - mel_min) * i as f32 / (N_MELS + 1) as f32))
        .collect();

    let bin_hz: Vec<f32> = (0..n_bins)
        .map(|i| i as f32 * SAMPLE_RATE / N_FFT as f32)
        .collect();

    let mut fb = vec![vec![0.0f32; n_bins]; N_MELS];
    for m in 0..N_MELS {
        let (lo, ctr, hi) = (points[m], points[m + 1], points[m + 2]);
        // Slaney normalisation: equal area per filter, not equal peak.
        let enorm = 2.0 / (hi - lo);
        for (b, &f) in bin_hz.iter().enumerate() {
            let w = if f >= lo && f <= ctr && ctr > lo {
                (f - lo) / (ctr - lo)
            } else if f > ctr && f <= hi && hi > ctr {
                (hi - f) / (hi - ctr)
            } else {
                0.0
            };
            fb[m][b] = w * enorm;
        }
    }
    fb
}

/// Periodic Hann, zero-padded symmetrically to `N_FFT` the way `torch.stft`
/// does when `win_length < n_fft`.
fn padded_window() -> Vec<f32> {
    let mut w = vec![0.0f32; N_FFT];
    let off = (N_FFT - WIN_LENGTH) / 2;
    for n in 0..WIN_LENGTH {
        w[off + n] = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * n as f32 / WIN_LENGTH as f32).cos();
    }
    w
}

/// Log-mel features in the encoder's layout: `[N_MELS * frames]`, mel-major.
pub fn log_mel(samples: &[f32]) -> (Vec<f32>, usize) {
    // Pre-emphasis.
    let mut x = Vec::with_capacity(samples.len());
    x.push(samples[0]);
    for i in 1..samples.len() {
        x.push(samples[i] - PREEMPH * samples[i - 1]);
    }

    // center=True with reflect padding.
    let pad = N_FFT / 2;
    let mut padded = Vec::with_capacity(x.len() + 2 * pad);
    for i in (1..=pad).rev() {
        padded.push(x[i.min(x.len() - 1)]);
    }
    padded.extend_from_slice(&x);
    for i in 1..=pad {
        padded.push(x[x.len().saturating_sub(1 + i)]);
    }

    let frames = (padded.len() - N_FFT) / HOP_LENGTH + 1;
    let window = padded_window();
    let fb = mel_filterbank();
    let n_bins = N_FFT / 2 + 1;

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N_FFT);

    let mut out = vec![0.0f32; N_MELS * frames];
    let mut buf = vec![Complex32::new(0.0, 0.0); N_FFT];
    let mut power = vec![0.0f32; n_bins];

    for t in 0..frames {
        let start = t * HOP_LENGTH;
        for i in 0..N_FFT {
            buf[i] = Complex32::new(padded[start + i] * window[i], 0.0);
        }
        fft.process(&mut buf);
        for b in 0..n_bins {
            power[b] = buf[b].re * buf[b].re + buf[b].im * buf[b].im;
        }
        for m in 0..N_MELS {
            let e: f32 = fb[m].iter().zip(&power).map(|(w, p)| w * p).sum();
            out[m * frames + t] = (e + LOG_GUARD).ln();
        }
    }

    // per_feature normalisation: zero mean, unit variance per mel bin over time.
    for m in 0..N_MELS {
        let row = &mut out[m * frames..(m + 1) * frames];
        let mean = row.iter().sum::<f32>() / frames as f32;
        let var =
            row.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / (frames as f32 - 1.0).max(1.0); // unbiased, as torch .std()
        let std = var.sqrt();
        for v in row.iter_mut() {
            *v = (*v - mean) / (std + NORM_EPS);
        }
    }

    (out, frames)
}

/// Read a mono 16 kHz WAV into normalised f32 samples.
pub fn read_wav(path: &str) -> anyhow::Result<Vec<f32>> {
    let mut r = hound::WavReader::open(path)?;
    let spec = r.spec();
    anyhow::ensure!(
        spec.sample_rate == 16_000,
        "expected 16 kHz, got {} Hz — resample first",
        spec.sample_rate
    );
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => r
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<Result<_, _>>()?,
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
    };
    // Downmix if the file is not already mono.
    Ok(if spec.channels > 1 {
        raw.chunks(spec.channels as usize)
            .map(|c| c.iter().sum::<f32>() / c.len() as f32)
            .collect()
    } else {
        raw
    })
}
