//! Transcribe a public meeting the way the app does after Stop, for the
//! memory gate.
//!
//! `memrun <16k.wav>`
//!
//! Puts `<16k.wav>` on both tracks of a fresh session in a temporary
//! directory and runs `session::transcribe_session`. Each helper stage prints
//! `ENGINE_PEAK <stage> <bytes>` and each phase `MARK <seconds> <phase>`;
//! `scripts/memory` gates on the largest peak, including this process's own.
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
    // `create_dir`, not `create_dir_all`: refuse a collision rather than reuse
    // an existing directory.
    std::fs::create_dir(&root)?;
    let dir = root.join("replay");
    std::fs::create_dir_all(dir.join("audio"))?;
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
