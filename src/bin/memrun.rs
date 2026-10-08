//! Replay a public meeting WAV through the recording path, for the memory gate.
//!
//! `memrun <native.wav> <16k.wav> [pace]`
//!
//! Streams `<native.wav>` from disk in 200 ms drains into a `LiveTranscriber`
//! on both tracks, paced at `pace`× realtime (default 15), then stops it and
//! finalizes the session with `session::transcribe_session`, exactly as the
//! app's queue does after Stop. `<16k.wav>` is the same audio at 16 kHz: the
//! capture path writes both, and finalization reads both.
//!
//! The input is read in drains, never whole, so the process holds what the app
//! holds and nothing more: `livecheck` keeps the whole file in memory twice,
//! which inflates its footprint by the file. `scripts/memory` runs this under
//! `/usr/bin/time -l` and compares the peak footprint with a budget.
//!
//! Each phase prints `MARK <seconds> <phase>` so a sampler can attribute what
//! it sees. Sessions, config and roster go to a temporary directory, removed
//! on success, so real sessions are untouched.
use ambient::{live::LiveTranscriber, session};
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: memrun <native.wav> <16k.wav> [pace]";
    let native = PathBuf::from(args.next().context(usage)?);
    let sixteen = PathBuf::from(args.next().context(usage)?);
    let pace: f64 = match args.next() {
        Some(p) => p.parse().context("pace is a number, e.g. 15")?,
        None => 15.0,
    };
    if !pace.is_finite() || pace <= 0.0 {
        bail!("pace must be positive");
    }

    let root = std::env::temp_dir().join(format!("ambient-memrun-{}", std::process::id()));
    // Refuse a collision rather than removing any existing directory.
    std::fs::create_dir(&root)?;
    let dir = root.join("replay");
    std::fs::create_dir_all(dir.join("audio"))?;
    // Isolate finalization's retention sweep and configuration from real sessions.
    std::env::set_var("AMBIENT_HOME", &root);
    std::env::set_var("AMBIENT_CONFIG", root.join("config.json"));
    std::env::set_var("AMBIENT_ROSTER", root.join("roster.json"));

    let mut reader =
        hound::WavReader::open(&native).with_context(|| format!("reading {}", native.display()))?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_format != hound::SampleFormat::Int
        || spec.bits_per_sample != 16
    {
        bail!("{} must be mono 16-bit PCM", native.display());
    }
    let rate = spec.sample_rate;
    let seconds = reader.duration() as f64 / rate as f64;

    let started = Instant::now();
    let mark = |phase: &str| println!("MARK {:.1} {phase}", started.elapsed().as_secs_f64());

    let (asr, vad) = session::model_paths(None)?;
    let mut worker = LiveTranscriber::start(&dir, [rate, rate], asr, vad)?;
    mark("record");
    let drain = rate as usize / 5;
    let mut fed = 0usize;
    let mut part = Vec::with_capacity(drain);
    let mut samples = reader.samples::<i16>();
    loop {
        part.clear();
        for s in samples.by_ref().take(drain) {
            part.push(s? as f32 / 32768.0);
        }
        if part.is_empty() {
            break;
        }
        worker.append(session::Track::Room, &part)?;
        worker.append(session::Track::Call, &part)?;
        fed += part.len();
        let due = Duration::from_secs_f64(fed as f64 / rate as f64 / pace);
        if let Some(wait) = due.checked_sub(started.elapsed()) {
            std::thread::sleep(wait);
        }
    }
    drop(reader);

    mark("stop");
    worker.finish()?;
    for track in ["room", "call"] {
        std::fs::copy(&native, dir.join(format!("audio/{track}.source.wav")))?;
        std::fs::copy(&sixteen, dir.join(format!("audio/{track}.wav")))?;
    }
    std::fs::write(
        dir.join("session.json"),
        serde_json::to_vec(&json!({
            "id":"replay","name":"Memory gate replay","started_at":"2026-09-21T00:00:00Z",
            "ended_at":"2026-09-21T00:30:00Z","duration_s":seconds,
            "device_hz":rate,"mic_hz":rate,"channels":1,"mic_channels":1,"apps":[],"model":"parakeet"
        }))?,
    )?;
    mark("finalize");
    session::transcribe_session(&dir, None, None)?;
    mark("idle");
    if !dir.join("transcript.md").exists() {
        bail!("finalization wrote no transcript.md in {}", dir.display());
    }
    std::fs::remove_dir_all(&root)?;
    println!(
        "MEMRUN OK: {seconds:.1}s at {rate} Hz on both tracks, {pace}x realtime, {:.1}s wall",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
