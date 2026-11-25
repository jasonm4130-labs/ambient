//! Transcribe a public meeting the way the app does after Stop, for the
//! memory gate.
//!
//! `memrun <16k.wav>`
//!
//! Puts `<16k.wav>` on both tracks of a fresh session and runs
//! `session::transcribe_session`, which runs each stage in `ambient-engine`
//! (ADR 0020). Nothing is transcribed while recording any more, so the
//! capture half holds no model and is not replayed. Each helper stage prints
//! `ENGINE_PEAK <stage> <bytes>` on stderr; `scripts/memory` gates on the
//! largest of those and of this process's own peak.
//!
//! Each phase prints `MARK <seconds> <phase>`. Sessions, config and roster go
//! to a temporary directory, removed on success, so real sessions are
//! untouched. The helper is `$AMBIENT_ENGINE` (scripts/memory builds it).
use ambient::session;
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let sixteen = PathBuf::from(args.next().context("usage: memrun <16k.wav>")?);
    let seconds = {
        let r = hound::WavReader::open(&sixteen)
            .with_context(|| format!("reading {}", sixteen.display()))?;
        let spec = r.spec();
        if spec.channels != 1 || spec.sample_rate != 16_000 {
            bail!("{} must be 16 kHz mono", sixteen.display());
        }
        r.duration() as f64 / 16_000.0
    };

    let root = std::env::temp_dir().join(format!("ambient-memrun-{}", std::process::id()));
    // Refuse a collision rather than removing any existing directory.
    std::fs::create_dir(&root)?;
    let dir = root.join("replay");
    std::fs::create_dir_all(dir.join("audio"))?;
    // Isolate the retention sweep and configuration from real sessions.
    std::env::set_var("AMBIENT_HOME", &root);
    std::env::set_var("AMBIENT_CONFIG", root.join("config.json"));
    std::env::set_var("AMBIENT_ROSTER", root.join("roster.json"));

    let started = Instant::now();
    let mark = |phase: &str| println!("MARK {:.1} {phase}", started.elapsed().as_secs_f64());
    for track in ["room", "call"] {
        std::fs::copy(&sixteen, dir.join(format!("audio/{track}.wav")))?;
    }
    std::fs::write(
        dir.join("session.json"),
        serde_json::to_vec(&json!({
            "id":"replay","name":"Memory gate replay","started_at":"2026-09-21T00:00:00Z",
            "ended_at":"2026-09-21T00:30:00Z","duration_s":seconds,
            "device_hz":16000,"mic_hz":16000,"channels":1,"mic_channels":1,"apps":[],
            "model":"replay"
        }))?,
    )?;
    mark("transcribe");
    session::transcribe_session(&dir, None)?;
    mark("idle");
    if !dir.join("transcript.md").exists() {
        bail!("transcription wrote no transcript.md in {}", dir.display());
    }
    std::fs::remove_dir_all(&root)?;
    println!(
        "MEMRUN OK: {seconds:.1}s on both tracks, {:.1}s wall",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
