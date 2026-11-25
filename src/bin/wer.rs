//! Word error rate for Ambient's own transcription path.
//!
//!   cargo run --release --bin wer -- [--manifest <path>] [--json <path>]
//!                                     [--via <rate>]
//!
//! Scores the `scripts/fetch-fixtures` fixture by running what the app runs,
//! `ambient-engine transcribe`, located the way the app locates it.
//!
//! `--via <rate>` transcodes each wav to `<rate>` Hz with ffmpeg and reads it
//! back through `resample::read_wav_any` and `resample::to_16k`, the path a
//! 48 or 44.1 kHz device takes. Transcodes are cached under
//! `~/.cache/ambient/fixtures/via/<rate>/<speaker>.wav`.

use ambient::{
    engine::{Stage, TranscriptFile},
    pipeline::Engine,
    resample,
    resample::TARGET_HZ,
    session, wer,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// `utterances` and `seconds` are deliberately unread: durations come from the
/// decoded audio, so a stale manifest cannot change a realtime factor.
#[derive(Deserialize)]
struct Entry {
    wav: PathBuf,
    reference: PathBuf,
    speaker: String,
    /// Set on the `calls` manifest, whose Earnings-21 reference covers the
    /// whole call while the fixture is its first 300 s; scored with
    /// `wer::score_prefix`.
    #[serde(default)]
    truncate_reference: bool,
}

/// The total row's WER is over summed counts, not averaged, so a short speaker
/// cannot outvote a long one.
#[derive(Serialize)]
struct Row {
    speaker: String,
    seconds: f64,
    reference_words: usize,
    substitutions: usize,
    insertions: usize,
    deletions: usize,
    wer: f64,
    decode_seconds: f64,
    realtime: f64,
}

fn default_manifest() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".cache/ambient/fixtures/wer/manifest.json")
}

fn via_cache_path(rate: u32, speaker: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join(".cache/ambient/fixtures/via")
        .join(rate.to_string())
        .join(format!("{speaker}.wav"))
}

/// `None` for anything unreadable, so a bad cache entry regenerates.
fn probed_rate(path: &Path) -> Option<u32> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=sample_rate",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// The cache directory also holds unrelated files; this touches only
/// `<speaker>.wav` and re-probes its rate before trusting it.
fn ensure_via(src: &Path, speaker: &str, rate: u32) -> Result<PathBuf> {
    let dst = via_cache_path(rate, speaker);
    if probed_rate(&dst) == Some(rate) {
        return Ok(dst);
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(src)
        .args(["-ar", &rate.to_string(), "-ac", "1", "-sample_fmt", "s16"])
        .arg(&dst)
        .status()
        .with_context(|| format!("running ffmpeg on {}", src.display()))?;
    if !status.success() {
        bail!("ffmpeg failed transcoding {} to {rate} Hz", src.display());
    }
    Ok(dst)
}

fn main() -> Result<()> {
    let mut manifest = None;
    let mut json = None;
    let mut via = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |flag: &str| args.next().with_context(|| format!("{flag} needs a path"));
        match a.as_str() {
            "--manifest" => manifest = Some(PathBuf::from(value("--manifest")?)),
            "--json" => json = Some(PathBuf::from(value("--json")?)),
            "--via" => {
                via = Some(
                    value("--via")?
                        .parse::<u32>()
                        .context("--via needs a sample rate in Hz, e.g. 48000")?,
                )
            }
            _ => bail!("usage: wer [--manifest <path>] [--json <path>] [--via <rate>]"),
        }
    }
    let manifest = manifest.unwrap_or_else(default_manifest);

    let raw = std::fs::read_to_string(&manifest).with_context(|| {
        format!(
            "no fixture manifest at {} — run scripts/fetch-fixtures to build it",
            manifest.display()
        )
    })?;
    let entries: Vec<Entry> =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", manifest.display()))?;
    if entries.is_empty() {
        bail!("{} lists no fixtures", manifest.display());
    }

    let engine = Engine::locate()?;
    let scratch = std::env::temp_dir().join(format!("ambient-wer-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;

    let mut rows: Vec<Row> = Vec::new();
    let mut total = wer::Wer::default();
    let (mut total_seconds, mut total_decode) = (0.0f64, 0.0f64);

    for entry in &entries {
        let wav = entry
            .wav
            .to_str()
            .with_context(|| format!("{} is not valid UTF-8", entry.wav.display()))?;
        let samples = if let Some(rate) = via {
            let transcoded = ensure_via(&entry.wav, &entry.speaker, rate)
                .with_context(|| format!("transcoding {wav} to {rate} Hz"))?;
            let (raw, actual_rate) = resample::read_wav_any(&transcoded)
                .with_context(|| format!("reading {}", transcoded.display()))?;
            resample::to_16k(&raw, actual_rate)
                .with_context(|| format!("resampling {} to 16 kHz", transcoded.display()))?
        } else {
            let (raw, rate) = resample::read_wav_any(&entry.wav)
                .with_context(|| format!("reading {} — run scripts/fetch-fixtures", wav))?;
            resample::to_16k(&raw, rate)?
        };
        let reference = std::fs::read_to_string(&entry.reference).with_context(|| {
            format!(
                "reading {} — run scripts/fetch-fixtures",
                entry.reference.display()
            )
        })?;
        let seconds = samples.len() as f64 / TARGET_HZ as f64;

        let track = scratch.join("track.wav");
        session::write_wav_16k(&track, &samples)?;
        // Includes model load, which the app also pays once per track.
        let t0 = Instant::now();
        let file: TranscriptFile =
            engine.once(Stage::Transcribe, &track, &scratch.join("words.json"))?;
        let decode = t0.elapsed().as_secs_f64();

        let hypothesis = file
            .words
            .iter()
            .map(|w| w.text.trim())
            .collect::<Vec<_>>()
            .join(" ");
        let reference_words = wer::normalise(&reference);
        let hypothesis_words = wer::normalise(&hypothesis);
        let scored = if entry.truncate_reference {
            wer::score_prefix(&reference_words, &hypothesis_words)
        } else {
            wer::score(&reference_words, &hypothesis_words)
        };

        total.substitutions += scored.substitutions;
        total.insertions += scored.insertions;
        total.deletions += scored.deletions;
        total.reference_words += scored.reference_words;
        total_seconds += seconds;
        total_decode += decode;

        rows.push(Row {
            speaker: entry.speaker.clone(),
            seconds,
            reference_words: scored.reference_words,
            substitutions: scored.substitutions,
            insertions: scored.insertions,
            deletions: scored.deletions,
            wer: scored.rate(),
            decode_seconds: decode,
            realtime: seconds / decode,
        });
    }

    rows.push(Row {
        speaker: "total".into(),
        seconds: total_seconds,
        reference_words: total.reference_words,
        substitutions: total.substitutions,
        insertions: total.insertions,
        deletions: total.deletions,
        wer: total.rate(),
        decode_seconds: total_decode,
        realtime: total_seconds / total_decode,
    });

    println!(
        "{:<9} {:>9} {:>7} {:>6} {:>6} {:>6} {:>8} {:>8} {:>9}",
        "speaker", "seconds", "words", "S", "I", "D", "WER", "decode", "realtime"
    );
    for r in &rows {
        println!(
            "{:<9} {:>9.2} {:>7} {:>6} {:>6} {:>6} {:>8.4} {:>8.2} {:>8.1}x",
            r.speaker,
            r.seconds,
            r.reference_words,
            r.substitutions,
            r.insertions,
            r.deletions,
            r.wer,
            r.decode_seconds,
            r.realtime
        );
    }

    std::fs::remove_dir_all(&scratch).ok();
    if let Some(path) = json {
        let body = serde_json::to_string_pretty(&rows)?;
        std::fs::write(&path, format!("{body}\n"))
            .with_context(|| format!("writing {}", path.display()))?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}
