//! Does transcribing beside a live capture starve the capture's drain loop?
//!
//! During back-to-back meetings the transcription queue runs ASR on CPU while
//! a new capture is being drained every 200 ms (ADR-0015). Audio is lost only
//! if the drain thread falls behind the tap's ring — 30 s on the mic, 60 s on
//! the call — so the number that matters is the *overshoot* of the drain
//! period while ASR runs, not the CPU it uses.
//!
//! This runs the same 200 ms sleep loop `capture_into` runs, on a default-QoS
//! thread, and measures each period's overshoot with ASR running on a
//! background-QoS thread exactly as `queue::Queue` spawns it. No tap is
//! needed: the drain loop is what we are measuring, and it does not touch the
//! tap while the ring is being written.
//!
//!   cargo run --release --bin drainbench -- [seconds] [wav]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ambient::{asr, resample, session, vad};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args.next().unwrap_or_else(|| "30".into()).parse()?;
    let wav = args
        .next()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            session::models_root()
                .unwrap_or_default()
                .join("sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8")
                .join("test_wavs")
                .join("en.wav")
        });
    let models = session::models_root()?;
    let asr_dir = models.join("sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8");
    let vad_path = models.join("silero_vad.onnx");

    let (raw, rate) = resample::read_wav_any(&wav)?;
    let samples = resample::to_16k(&raw, rate)?;
    println!(
        "asr input: {} ({:.1}s), transcribed in a loop for {seconds}s",
        wav.display(),
        samples.len() as f64 / 16_000.0
    );

    // The drain loop alone first, so the ASR numbers have something to be
    // compared with on this machine.
    let idle = measure(seconds.min(5), &AtomicBool::new(false));
    println!("\nidle (no ASR):      {}", idle);

    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let stop = stop.clone();
        let asr_dir = asr_dir.to_string_lossy().into_owned();
        let vad_path = vad_path.to_string_lossy().into_owned();
        std::thread::spawn(move || -> anyhow::Result<usize> {
            // Exactly what `queue::Queue::spawn` sets.
            #[cfg(target_os = "macos")]
            unsafe {
                libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
            }
            let mut vad = vad::Vad::load(&vad_path)?;
            let mut rec = asr::Recognizer::load(&asr_dir)?;
            let chunks = vad.turns(&samples, 30)?;
            let mut passes = 0;
            while !stop.load(Ordering::Relaxed) {
                let _ = rec.transcribe_segments(&samples, &chunks)?;
                passes += 1;
            }
            Ok(passes)
        })
    };
    // Let the models load so the measurement is of decoding, not disk.
    std::thread::sleep(Duration::from_secs(3));
    let under_asr = measure(seconds, &stop);
    stop.store(true, Ordering::Relaxed);
    let passes = worker.join().expect("asr thread panicked")?;
    println!("under ASR:          {under_asr}");
    println!("asr passes:         {passes}");

    // The verdict, against the smaller of the two rings.
    let budget = Duration::from_secs(30);
    let verdict = if under_asr.max < budget / 100 {
        "ok — worst overshoot is under 1% of the 30 s ring"
    } else if under_asr.max < budget {
        "marginal — audio survives, but look at why the drain loop stalled"
    } else {
        "FAIL — the drain loop fell behind the ring; audio would be lost"
    };
    println!("\nring budget:        30s (mic), 60s (call)\nverdict:            {verdict}");
    Ok(())
}

/// Overshoot statistics for a 200 ms sleep loop run for `seconds`.
struct Overshoot {
    periods: usize,
    max: Duration,
    p99: Duration,
    mean: Duration,
}

impl std::fmt::Display for Overshoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} periods, overshoot mean {:.2} ms, p99 {:.2} ms, max {:.2} ms",
            self.periods,
            self.mean.as_secs_f64() * 1e3,
            self.p99.as_secs_f64() * 1e3,
            self.max.as_secs_f64() * 1e3
        )
    }
}

fn measure(seconds: u64, stop: &AtomicBool) -> Overshoot {
    let period = Duration::from_millis(200);
    let end = Instant::now() + Duration::from_secs(seconds);
    let mut overshoots = Vec::new();
    while Instant::now() < end && !stop.load(Ordering::Relaxed) {
        let t = Instant::now();
        std::thread::sleep(period);
        overshoots.push(t.elapsed().saturating_sub(period));
    }
    overshoots.sort();
    let n = overshoots.len().max(1);
    Overshoot {
        periods: overshoots.len(),
        max: overshoots.last().copied().unwrap_or_default(),
        p99: overshoots
            .get((n * 99 / 100).min(n - 1))
            .copied()
            .unwrap_or_default(),
        mean: overshoots.iter().sum::<Duration>() / n as u32,
    }
}
