//! Word error rate for Ambient's own transcription path.
//!
//!   cargo run --release --bin wer -- [--manifest <path>] [--json <path>]
//!                                     [--model <dir>] [--via <rate>]
//!
//! Scores the fixture `scripts/fetch-fixtures` builds by running what
//! `ambient record` runs for a finished track: the same model resolution, the
//! same VAD turns, the same `transcribe_segments`. A harness with its own
//! decode path would measure a pipeline no user has.
//!
//! `--model` overrides only the ASR directory, through the same
//! `session::model_paths` a `record --model` goes through, so comparing two
//! shipped models compares them at the one place the product chooses one.
//!
//! `--via <rate>` puts `resample::to_16k` in the path the fixture would
//! otherwise skip: the 16 kHz wav is upsampled to `rate` with `ffmpeg` once and
//! cached, then read back and resampled down here. Capture writes native-rate
//! bytes — 48 kHz from Core Audio — so a user's audio always crosses that
//! function and the fixture never did.

use ambient::{asr::Recognizer, features, resample, resample::TARGET_HZ, session, vad::Vad, wer};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// One fixture as `scripts/fetch-fixtures` writes it. `utterances` and
/// `seconds` are in the manifest too and deliberately unread: the durations
/// below come from the audio actually decoded, so a stale manifest cannot
/// quietly change a realtime factor.
#[derive(Deserialize)]
struct Entry {
    wav: PathBuf,
    reference: PathBuf,
    speaker: String,
}

/// One printed row, and one element of the `--json` array. The last row is the
/// total, with `speaker` reading `total`; its WER is computed over the summed
/// counts, not averaged over the rows, so a short speaker cannot outvote a
/// long one.
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
    cache_root().join("fixtures/wer/manifest.json")
}

fn cache_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".cache/ambient")
}

/// Upsample `wav` to `rate` with `ffmpeg`, cached under
/// `~/.cache/ambient/fixtures/via/<rate>/`, and return the cached path.
///
/// Cached because the upsample is deterministic and slow, and because the point
/// of the run is `resample::to_16k`, not ffmpeg. Written to a `.part` file and
/// renamed, so an interrupted run leaves no truncated wav that a later run
/// would happily read as the fixture.
fn upsample(wav: &Path, speaker: &str, rate: u32) -> Result<PathBuf> {
    let dir = cache_root().join("fixtures/via").join(rate.to_string());
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let out = dir.join(format!("{speaker}.wav"));
    if out.exists() {
        return Ok(out);
    }
    let part = out.with_extension("part");
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(wav)
        .args([
            "-ar",
            &rate.to_string(),
            "-ac",
            "1",
            "-c:a",
            "pcm_s16le",
            "-f",
            "wav",
        ])
        .arg(&part)
        .status()
        .context("running ffmpeg — --via needs it on PATH (brew install ffmpeg)")?;
    if !status.success() {
        let _ = std::fs::remove_file(&part);
        bail!("ffmpeg failed to upsample {} to {rate} Hz", wav.display());
    }
    std::fs::rename(&part, &out).with_context(|| format!("writing {}", out.display()))?;
    println!("upsampled {} -> {}", wav.display(), out.display());
    Ok(out)
}

fn main() -> Result<()> {
    let mut manifest = None;
    let mut json = None;
    let mut model = None;
    let mut via: Option<u32> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |flag: &str| args.next().with_context(|| format!("{flag} needs a path"));
        match a.as_str() {
            "--manifest" => manifest = Some(PathBuf::from(value("--manifest")?)),
            "--json" => json = Some(PathBuf::from(value("--json")?)),
            "--model" => model = Some(value("--model")?),
            "--via" => {
                let raw = value("--via")?;
                via = Some(
                    raw.parse()
                        .with_context(|| format!("--via wants a sample rate in Hz, got {raw}"))?,
                );
            }
            _ => bail!(
                "usage: wer [--manifest <path>] [--json <path>] [--model <dir>] [--via <rate>]"
            ),
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

    // The one resolution `record` uses, not a second copy: a harness pointed at
    // a different model measures a different product.
    let (asr_dir, vad_path) = session::model_paths(model.as_deref())
        .context("the models this run needs are missing — run ./fetch-models.sh")?;
    let asr_dir = asr_dir
        .to_str()
        .context("ASR model path is not valid UTF-8")?
        .to_string();
    let vad_path = vad_path
        .to_str()
        .context("VAD model path is not valid UTF-8")?
        .to_string();

    let mut rows: Vec<Row> = Vec::new();
    let mut total = wer::Wer::default();
    let (mut total_seconds, mut total_decode) = (0.0f64, 0.0f64);

    for entry in &entries {
        let wav = entry
            .wav
            .to_str()
            .with_context(|| format!("{} is not valid UTF-8", entry.wav.display()))?;
        let samples = match via {
            None => features::read_wav(wav)
                .with_context(|| format!("reading {} — run scripts/fetch-fixtures", wav))?,
            Some(rate) => {
                let up = upsample(&entry.wav, &entry.speaker, rate)?;
                let (raw, got) = resample::read_wav_any(&up)?;
                // A cache file at the wrong rate would silently measure a
                // different conversion than the one named on the command line.
                if got != rate {
                    bail!("{} is {got} Hz, expected {rate} Hz", up.display());
                }
                resample::to_16k(&raw, got)?
            }
        };
        let reference = std::fs::read_to_string(&entry.reference).with_context(|| {
            format!(
                "reading {} — run scripts/fetch-fixtures",
                entry.reference.display()
            )
        })?;
        let seconds = samples.len() as f64 / TARGET_HZ as f64;

        let mut vad = Vad::load(&vad_path)?;
        let mut rec = Recognizer::load(&asr_dir)?;

        // The clock starts after the models load: a load is a fixed cost per
        // session, and the realtime factor is about seconds of audio.
        let t0 = Instant::now();
        let turns = vad.turns(&samples, 30)?;
        let segments = rec.transcribe_segments(&samples, &turns)?;
        let decode = t0.elapsed().as_secs_f64();

        let hypothesis = segments
            .iter()
            .map(|(_, text, _)| text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let scored = wer::score(&wer::normalise(&reference), &wer::normalise(&hypothesis));

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

    match via {
        None => println!("read at 16 kHz, resample::to_16k not exercised"),
        Some(rate) => println!("read at {rate} Hz through resample::to_16k"),
    }
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

    if let Some(path) = json {
        let body = serde_json::to_string_pretty(&rows)?;
        std::fs::write(&path, format!("{body}\n"))
            .with_context(|| format!("writing {}", path.display()))?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}
