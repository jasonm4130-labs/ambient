//! Sample-rate conversion to the 16 kHz everything downstream expects.
//!
//! The capture layer hands back audio at whatever rate the device runs — 48 kHz
//! on this Mac, 44.1 kHz on some interfaces — while `features::read_wav` refuses
//! anything but 16 kHz. This is the piece that was missing between them, so the
//! tap and the transcriber had never actually met.
//!
//! Resampling belongs to the *replayable* half of the pipeline. Recording
//! happens once and cannot be redone; transcription can be rerun as often as we
//! like. So capture writes native-rate bytes and this runs afterwards, which
//! means a bug here costs a rerun rather than the conversation.

use anyhow::{anyhow, Result};
use rubato::audioadapter_buffers::owned::InterleavedOwned;
use rubato::{Fft, FixedSync, Resampler};

pub const TARGET_HZ: u32 = 16_000;

/// Chunk size handed to the FFT resampler. Only affects internal blocking.
const CHUNK: usize = 1024;

/// Resample mono `samples` from `from_hz` to 16 kHz.
///
/// Uses rubato's synchronous FFT resampler, which is band-limited — a plain
/// "take every third sample" decimation would alias everything above 8 kHz back
/// down into the speech band, and Parakeet would transcribe the result fluently
/// and wrongly rather than fail.
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

/// Read a WAV at whatever rate and channel count it happens to be, downmixed to
/// mono. The companion to [`to_16k`]: together they let anything on disk reach
/// the recogniser, which accepts 16 kHz mono and nothing else.
pub fn read_wav_any(path: &std::path::Path) -> Result<(Vec<f32>, u32)> {
    let mut r = hound::WavReader::open(path)
        .map_err(|e| anyhow!("opening {}: {e}", path.display()))?;
    let spec = r.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => r
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<std::result::Result<_, _>>()?,
        hound::SampleFormat::Float => {
            r.samples::<f32>().collect::<std::result::Result<_, _>>()?
        }
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

/// Reduce an interleaved frame to one mono channel by averaging the channels in
/// `range`. Averaging rather than summing keeps the result inside [-1, 1] when
/// two correlated channels carry the same voice.
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

    /// A 1 kHz tone must survive 48 -> 16 kHz: same duration, same amplitude,
    /// still periodic. This catches a resampler that silently outputs silence
    /// or garbage, which is the failure mode that would otherwise reach the ASR
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
        // two frames, three channels
        let inter = vec![1.0, 0.0, 9.0, 0.5, 0.5, 9.0];
        assert_eq!(downmix(&inter, 3, 0..2), vec![0.5, 0.5]);
        assert_eq!(downmix(&inter, 3, 2..3), vec![9.0, 9.0]);
    }
}
