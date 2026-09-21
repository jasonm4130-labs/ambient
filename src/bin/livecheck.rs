//! Replay a public speech WAV through live transcription and MCP, without a tap.
//!
//! `cargo run --bin livecheck -- /path/to/public-fixture.wav`
use ambient::{live::LiveTranscriber, resample, session};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn poll(root: &Path, since: u64) -> Result<Value> {
    let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"transcript","arguments":{"session":"replay","since":since}}});
    let mut output = Vec::new();
    ambient::mcp::serve(format!("{request}\n").as_bytes(), &mut output, root)?;
    let response: Value = serde_json::from_slice(&output)?;
    let result = &response["result"];
    if result["isError"] == true || response.get("error").is_some() {
        bail!("MCP replay read failed: {response}");
    }
    serde_json::from_str(result["content"][0]["text"].as_str().context("MCP text")?)
        .context("MCP transcript JSON")
}

fn write_wav(path: &Path, samples: &[f32], rate: u32) -> Result<()> {
    let mut wav = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )?;
    for sample in samples {
        wav.write_sample((sample.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    wav.finalize()?;
    Ok(())
}

fn main() -> Result<()> {
    let input = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .context("usage: livecheck <public-speech-fixture.wav>")?;
    let root = std::env::temp_dir().join(format!("ambient-livecheck-{}", std::process::id()));
    // Refuse a collision rather than removing any existing directory.
    std::fs::create_dir(&root)?;
    let dir = root.join("replay");
    std::fs::create_dir(&dir)?;
    std::fs::create_dir(dir.join("audio"))?;
    // Isolate finalization's retention sweep and configuration from real sessions.
    std::env::set_var("AMBIENT_HOME", &root);
    std::env::set_var("AMBIENT_CONFIG", root.join("config.json"));
    std::env::set_var("AMBIENT_ROSTER", root.join("roster.json"));
    let (samples, rate) = resample::read_wav_any(&input)?;
    if samples.len() < rate as usize * 35 {
        bail!("use a public speech fixture at least 35 seconds long");
    }
    let (asr, vad) = session::model_paths(None)?;
    let mut worker = LiveTranscriber::start(&dir, [rate, rate], asr, vad)?;
    let split = samples.len().min(rate as usize * 30);
    let started = Instant::now();
    for part in samples[..split].chunks(rate as usize / 5) {
        worker.append(session::Track::Room, part)?;
        worker.append(session::Track::Call, part)?;
    }
    let first = loop {
        let reply = poll(&root, 0)?;
        if reply["next"].as_u64().unwrap_or(0) > 0 {
            break reply;
        }
        if started.elapsed() > Duration::from_secs(120) {
            bail!("no transcript before source EOF");
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let cursor = first["next"].as_u64().context("cursor")?;
    let first_seconds = started.elapsed().as_secs_f64();
    let prefix = first["lines"].as_array().context("first lines")?.clone();
    for part in samples[split..].chunks(rate as usize / 5) {
        worker.append(session::Track::Room, part)?;
        worker.append(session::Track::Call, part)?;
    }
    for track in ["room", "call"] {
        write_wav(
            &dir.join(format!("audio/{track}.native.wav")),
            &samples,
            rate,
        )?;
    }
    worker.finish()?;
    let sixteen = resample::to_16k(&samples, rate)?;
    for track in ["room", "call"] {
        write_wav(
            &dir.join(format!("audio/{track}.wav")),
            &sixteen,
            resample::TARGET_HZ,
        )?;
        std::fs::rename(
            dir.join(format!("audio/{track}.native.wav")),
            dir.join(format!("audio/{track}.source.wav")),
        )?;
    }
    std::fs::write(
        dir.join("session.json"),
        serde_json::to_vec(&json!({
            "id":"replay","name":"Public fixture replay","started_at":"2026-09-21T00:00:00Z",
            "ended_at":"2026-09-21T00:02:00Z","duration_s":samples.len() as f64 / rate as f64,
            "device_hz":rate,"mic_hz":rate,"channels":1,"mic_channels":1,"apps":[],"model":"parakeet"
        }))?,
    )?;
    session::transcribe_session(&dir, None, None)?;
    for track in ["room", "call"] {
        for suffix in ["source.wav", "live.pcm"] {
            if dir.join(format!("audio/{track}.{suffix}")).exists() {
                bail!("finalization retained temporary {track}.{suffix}");
            }
        }
    }
    let all = poll(&root, 0)?;
    let lines = all["lines"].as_array().context("final lines")?;
    // Speaker labels may be added at finalization; words and identities must not change.
    for (before, after) in prefix.iter().zip(lines.iter()) {
        for key in ["track", "start_ms", "end_ms", "text"] {
            if before[key] != after[key] {
                bail!("published prefix changed at {key}");
            }
        }
    }
    let unseen = poll(&root, cursor)?;
    if all["state"] != "done"
        || lines.len() < prefix.len()
        || unseen["lines"].as_array().context("unseen lines")?.len() != lines.len() - prefix.len()
    {
        bail!("final MCP cursor/state mismatch");
    }
    let raw = std::fs::read(dir.join("raw.jsonl"))?;
    session::transcribe_session(&dir, None, None)?;
    if std::fs::read(dir.join("raw.jsonl"))? != raw {
        bail!("retry changed raw transcript");
    }
    println!("LIVE CHECK OK: {cursor} lines before EOF in {first_seconds:.2}s; {} final lines; stable MCP cursor and retry; source {:.2}s; {}",
        lines.len(), samples.len() as f64 / rate as f64, dir.display());
    Ok(())
}
