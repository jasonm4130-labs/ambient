//! Diarize a bare wav and print the spans — the tuning tool for `--threshold`,
//! and the only place the powerset class layout is directly observable.
//!
//!   diartest <segmentation.onnx> <embedding.onnx> <a.wav> [threshold]
//!
//! `AMBIENT_DEBUG_DIAR=1` adds the per-window class histogram.

use anyhow::{bail, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        bail!("usage: diartest <segmentation.onnx> <embedding.onnx> <a.wav> [threshold]");
    }
    let threshold = args
        .get(3)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(ambient::diarize::DEFAULT_THRESHOLD);

    let (raw, rate) = ambient::resample::read_wav_any(std::path::Path::new(&args[2]))?;
    let samples = ambient::resample::to_16k(&raw, rate)?;
    eprintln!(
        "{:.1}s at {rate} Hz, threshold {threshold}",
        samples.len() as f64 / 16_000.0
    );

    let mut d = ambient::diarize::Diarizer::load(&args[0], &args[1])?;
    let spans = d.diarize(&samples, threshold)?;

    let n = spans.iter().map(|s| s.speaker).max().map(|m| m + 1).unwrap_or(0);
    println!("{} span(s), {n} speaker(s)", spans.len());
    for s in &spans {
        println!(
            "  {:>7.2}s {:>7.2}s  speaker {}",
            s.start as f64 / 16_000.0,
            s.end as f64 / 16_000.0,
            s.speaker
        );
    }
    Ok(())
}
