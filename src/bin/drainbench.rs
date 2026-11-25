//! Does transcribing beside a live capture starve the capture's drain loop?
//!
//! Audio is lost only if the 200 ms drain falls behind the tap's ring (30 s
//! mic, 60 s call), so this measures the drain period's overshoot on a
//! default-QoS thread while a background-QoS thread runs the helper's stages
//! over a 16 kHz mono wav. No tap is needed.
//!
//!   cargo run --release --bin drainbench -- [seconds] [16k-mono.wav]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::bail;

use ambient::{
    engine::{SpeakersFile, Stage, TranscriptFile},
    pipeline::Engine,
};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args.next().unwrap_or_else(|| "30".into()).parse()?;
    let wav = match args.next() {
        Some(w) => std::path::PathBuf::from(w),
        None => {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            std::path::PathBuf::from(home).join(".cache/ambient/fixtures/memory/TS3003a.16k.wav")
        }
    };
    if !wav.is_file() {
        bail!(
            "no {} — pass a 16 kHz mono wav, or run scripts/memory once",
            wav.display()
        );
    }
    let engine = Engine::locate()?;
    let scratch = std::env::temp_dir().join(format!("ambient-drainbench-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    println!(
        "model input: {}, transcribed and diarized in a loop for {seconds}s",
        wav.display()
    );

    let idle = measure(seconds.min(5), &AtomicBool::new(false));
    println!("\nidle (no models):   {}", idle);

    let stop = Arc::new(AtomicBool::new(false));
    let passes = Arc::new(AtomicUsize::new(0));
    let worker = {
        let stop = stop.clone();
        let passes = passes.clone();
        let scratch = scratch.clone();
        std::thread::spawn(move || -> anyhow::Result<()> {
            #[cfg(target_os = "macos")]
            unsafe {
                libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
            }
            while !stop.load(Ordering::Relaxed) {
                let _: TranscriptFile =
                    engine.once(Stage::Transcribe, &wav, &scratch.join("words.json"))?;
                let _: SpeakersFile =
                    engine.once(Stage::Diarize, &wav, &scratch.join("speakers.json"))?;
                passes.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        })
    };
    let before = passes.load(Ordering::Relaxed);
    let under_asr = measure(seconds, &stop);
    let after = passes.load(Ordering::Relaxed);
    stop.store(true, Ordering::Relaxed);
    worker.join().expect("model thread panicked")?;
    std::fs::remove_dir_all(&scratch).ok();
    println!("under the models:   {under_asr}");
    println!("passes completed:   {}", after - before);

    // Against the smaller ring.
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
