//! Does the fbank front-end actually produce usable speaker embeddings?
//!
//! A wrong filterbank does not fail here — it yields embeddings that still
//! cluster, just badly, and the damage only shows up later as confidently
//! mislabelled speakers. So compare same-voice against different-voice cosine
//! similarity directly: if the front-end is right the two groups separate
//! clearly, and if it is wrong the matrix collapses toward uniform.
//!
//!   embtest <wespeaker.onnx> <a1.wav> <a2.wav> <b1.wav> <b2.wav>
//!
//! Expects the first two clips to be one voice and the last two another.

use anyhow::{bail, Result};
use ort::session::Session;
use ort::value::Tensor;

trait OrtExt<T> {
    fn a(self) -> Result<T>;
}
impl<T> OrtExt<T> for ort::Result<T> {
    fn a(self) -> Result<T> {
        self.map_err(|e| anyhow::anyhow!(e.to_string()))
    }
}

fn embed(s: &mut Session, samples: &[f32]) -> Result<Vec<f32>> {
    let (feats, frames) = ambient::fbank::fbank(samples);
    if frames == 0 {
        bail!("clip too short for a single frame");
    }
    let t = Tensor::from_array((
        vec![1_i64, frames as i64, ambient::fbank::N_MELS as i64],
        feats,
    ))
    .a()?;
    let out = s.run(ort::inputs!["feats" => &t]).a()?;
    let (_, e) = out["embs"].try_extract_tensor::<f32>().a()?;
    // L2-normalise so a dot product is the cosine similarity.
    let norm = e.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-9);
    Ok(e.iter().map(|v| v / norm).collect())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 5 {
        bail!("usage: embtest <wespeaker.onnx> <a1.wav> <a2.wav> <b1.wav> <b2.wav>");
    }
    let mut s = Session::builder().a()?.commit_from_file(&args[0]).a()?;

    let mut embs = Vec::new();
    for p in &args[1..] {
        let (raw, rate) = ambient::resample::read_wav_any(std::path::Path::new(p))?;
        let samples = ambient::resample::to_16k(&raw, rate)?;
        embs.push(embed(&mut s, &samples)?);
    }

    let names = ["A1", "A2", "B1", "B2"];
    println!("cosine similarity");
    print!("      ");
    for n in &names {
        print!("{n:>7}");
    }
    println!();
    for (i, n) in names.iter().enumerate() {
        print!("{n:>6}");
        for j in 0..4 {
            let d: f32 = embs[i].iter().zip(&embs[j]).map(|(a, b)| a * b).sum();
            print!("{d:>7.3}");
        }
        println!();
    }

    let same =
        |i: usize, j: usize| -> f32 { embs[i].iter().zip(&embs[j]).map(|(a, b)| a * b).sum() };
    let within = (same(0, 1) + same(2, 3)) / 2.0;
    let across = (same(0, 2) + same(0, 3) + same(1, 2) + same(1, 3)) / 4.0;
    println!("\nsame voice   {within:.3}");
    println!("cross voice  {across:.3}");
    println!("separation   {:.3}", within - across);
    if within - across < 0.25 {
        println!("\nFAIL: voices are not separating — suspect the fbank front-end.");
        std::process::exit(1);
    }
    println!("\nPASS");
    Ok(())
}
