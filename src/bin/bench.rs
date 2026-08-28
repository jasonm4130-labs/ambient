//! Phase 0 measurement: does the Parakeet encoder actually run on the Neural
//! Engine under ONNX Runtime's CoreML provider, or does it silently fall back?
//!
//! EP availability is not graph placement. The discriminator is wall clock: if
//! the CoreML run matches the CPU-only run, it fell back.
//!
//!   cargo run --release --bin bench -- <encoder.onnx> <coreml|cpu> [seconds]

use ort::ep::coreml::ComputeUnits;
use ort::ep::CoreML;
use ort::session::Session;
use ort::value::Tensor;
use std::time::Instant;

/// Peak resident set size for this process, in MB.
fn peak_rss_mb() -> f64 {
    unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut ru);
        ru.ru_maxrss as f64 / 1_048_576.0 // macOS reports bytes
    }
}

fn main() -> ort::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: bench <encoder.onnx> <coreml|cpu> [seconds]");
    let ep = args.next().unwrap_or_else(|| "coreml".into());
    let seconds: usize = args.next().unwrap_or_else(|| "60".into()).parse().unwrap();

    // NeMo front-end: 128 mel bins, 10 ms hop -> 100 frames per second of audio.
    let frames = seconds * 100;

    let mut builder = Session::builder()?;
    if ep == "coreml" {
        builder = builder.with_execution_providers([CoreML::default()
            .with_compute_units(ComputeUnits::CPUAndNeuralEngine)
            .build()])?;
    }

    let load = Instant::now();
    let mut session = builder.commit_from_file(&path)?;
    let load_ms = load.elapsed().as_millis();

    let signal = Tensor::from_array((vec![1_i64, 128, frames as i64], vec![0.0_f32; 128 * frames]))?;
    let length = Tensor::from_array((vec![1_i64], vec![frames as i64]))?;

    // Warm-up: the first CoreML run pays model compilation.
    let warm = Instant::now();
    let _ = session.run(ort::inputs!["audio_signal" => &signal, "length" => &length])?;
    let warm_ms = warm.elapsed().as_millis();

    let runs = 3;
    let t = Instant::now();
    for _ in 0..runs {
        let _ = session.run(ort::inputs!["audio_signal" => &signal, "length" => &length])?;
    }
    let per_run = t.elapsed().as_secs_f64() / runs as f64;

    println!("ep            {ep}");
    println!("audio         {seconds}s ({frames} mel frames)");
    println!("load          {load_ms} ms");
    println!("first run     {warm_ms} ms (includes any CoreML compile)");
    println!("steady run    {:.0} ms", per_run * 1000.0);
    println!("realtime      {:.0}x", seconds as f64 / per_run);
    println!("peak rss      {:.0} MB", peak_rss_mb());
    Ok(())
}
