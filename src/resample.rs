//! Resampling to the 16 kHz mono the model helper expects. It runs after
//! capture, on native-rate files, so a bug here costs a rerun, not a recording.

use anyhow::{anyhow, Result};
use rubato::audioadapter_buffers::owned::InterleavedOwned;
use rubato::{Fft, FixedSync, Resampler};

pub const TARGET_HZ: u32 = 16_000;

/// FFT block size; affects internal blocking only, not the output.
const CHUNK: usize = 1024;

/// Band-limited, because plain decimation would alias everything above 8 kHz
/// into the speech band and the recogniser would transcribe it fluently and
/// wrongly.
pub fn to_16k(samples: &[f32], from_hz: u32) -> Result<Vec<f32>> {
    if from_hz == TARGET_HZ {
        return Ok(samples.to_vec());
    }
    if samples.is_empty() {
        return Ok(Vec::new());
    }
    if from_hz == 0 {
        return Err(anyhow!("input sample rate is zero"));
    }

    let frames = samples.len();
    let input = InterleavedOwned::<f32>::new_from(samples.to_vec(), 1, frames)
        .map_err(|e| anyhow!("input buffer: {e}"))?;

    let mut r = Fft::<f32>::new(
        from_hz as usize,
        TARGET_HZ as usize,
        CHUNK,
        1,
        FixedSync::Both,
    )
    .map_err(|e| anyhow!("resampler {from_hz} -> {TARGET_HZ}: {e}"))?;

    // Sized for the worst case; `produced` says how much is real.
    let need = r.process_all_needed_output_len(frames);
    let mut output = InterleavedOwned::<f32>::new(0.0f32, 1, need);

    let (_consumed, produced) = r
        .process_all_into_buffer(&input, &mut output, frames, None)
        .map_err(|e| anyhow!("resample: {e}"))?;

    let mut out = output.take_data();
    out.truncate(produced);
    Ok(out)
}

/// Any WAV, downmixed to mono; returns the samples and their rate.
pub fn read_wav_any(path: &std::path::Path) -> Result<(Vec<f32>, u32)> {
    let mut r =
        hound::WavReader::open(path).map_err(|e| anyhow!("opening {}: {e}", path.display()))?;
    let spec = r.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => r
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<std::result::Result<_, _>>()?,
        hound::SampleFormat::Float => r.samples::<f32>().collect::<std::result::Result<_, _>>()?,
    };
    let mono = if spec.channels > 1 {
        raw.chunks(spec.channels as usize)
            .map(|c| c.iter().sum::<f32>() / c.len() as f32)
            .collect()
    } else {
        raw
    };
    Ok((mono, spec.sample_rate))
}

/// Averages the channels in `range`; summing would push correlated channels
/// outside [-1, 1].
pub fn downmix(interleaved: &[f32], channels: usize, range: std::ops::Range<usize>) -> Vec<f32> {
    if channels == 0 || range.is_empty() {
        return Vec::new();
    }
    let n = range.len() as f32;
    let frames = interleaved.len() / channels;
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let base = f * channels;
        let mut acc = 0.0f32;
        for c in range.clone() {
            acc += interleaved[base + c];
        }
        out.push(acc / n);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resampler that outputs silence or garbage would reach the recogniser
    /// as fluent nonsense.
    #[test]
    fn tone_survives_downsampling() {
        let src: Vec<f32> = (0..48_000)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48_000.0).sin())
            .collect();
        let out = to_16k(&src, 48_000).unwrap();

        let ratio = out.len() as f64 / 16_000.0;
        assert!(ratio > 0.98 && ratio < 1.02, "length {} frames", out.len());

        // Ignore the edges, where the anti-alias window rolls in and out.
        let mid = &out[1600..out.len() - 1600];
        let peak = mid.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        assert!(peak > 0.9 && peak <= 1.01, "peak {peak}");
    }

    #[test]
    fn passthrough_at_target_rate() {
        let src = vec![0.25f32; 800];
        assert_eq!(to_16k(&src, 16_000).unwrap(), src);
    }

    #[test]
    fn downmix_averages_the_named_channels() {
        let inter = vec![1.0, 0.0, 9.0, 0.5, 0.5, 9.0];
        assert_eq!(downmix(&inter, 3, 0..2), vec![0.5, 0.5]);
        assert_eq!(downmix(&inter, 3, 2..3), vec![9.0, 9.0]);
    }
}
