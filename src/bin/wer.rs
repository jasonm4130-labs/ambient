//! Word error rate for Ambient's own transcription path.
//!
//!   cargo run --release --bin wer -- [--manifest <path>] [--json <path>]
//!                                     [--model <dir>] [--via <rate>] [--pad <ms>]
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
//! `--via <rate>` transcodes each fixture wav to `<rate>` Hz with ffmpeg first,
//! then reads it back through `resample::read_wav_any` and `resample::to_16k`
//! — the same downsample path Core Audio's 48 kHz (or an interface's 44.1 kHz)
//! delivery takes before `features::read_wav`'s hard 16 kHz requirement could
//! ever see it. Transcodes are cached under
//! `~/.cache/ambient/fixtures/via/<rate>/<speaker>.wav` and reused only when
//! `ffprobe` confirms the cached file is still at `<rate>`.
//!
//! A manifest entry with `"truncate_reference": true` — the `calls` set,
//! built by `scripts/fetch-fixtures` from Earnings-21 conference-bridge
//! audio — is scored with `wer::score_prefix` instead of `wer::score`: its
//! reference is the whole call's human transcript, but the fixture wav is
//! only the call's first 300 s, so scoring against the whole reference would
//! count everything spoken after the cut as deletions. Manifests without the
//! field, including the default `wer` one, are unaffected.
//!
//! `--pad <ms>` overrides `Vad::pad_ms`, which defaults to `vad::PAD_MS`
//! (200 ms) — the margin `trim_quiet` re-applies after cutting quiet edges
//! and the hysteresis pad `segments_from` applies before merging, both in
//! `src/vad.rs`. It reaches both sites through the one `Vad` in this
//! process; nothing else constructs one.

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
    /// True on the `calls` manifest only: the Earnings-21 reference covers
    /// the whole call but the fixture wav is only its first 300 s, so the
    /// reference is scored against `wer::score_prefix` rather than
    /// `wer::score` — the reference's best-matching prefix, not all of it.
    /// Defaulted so the `wer` and `der`-adjacent manifests, which carry no
    /// such field, keep scoring exactly as before.
    #[serde(default)]
    truncate_reference: bool,
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
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".cache/ambient/fixtures/wer/manifest.json")
}

/// Where the `<rate>` Hz transcode of `speaker`'s fixture lives.
fn via_cache_path(rate: u32, speaker: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join(".cache/ambient/fixtures/via")
        .join(rate.to_string())
        .join(format!("{speaker}.wav"))
}

/// The sample rate `ffprobe` reports for `path`, or `None` if it cannot be
/// read — a file that is missing, unreadable or from a stale layout should
/// regenerate rather than pretend to match.
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

/// Transcode `src` to `rate` Hz mono 16-bit with ffmpeg, reusing a cached
/// transcode when one already exists at the right rate. The cache directory
/// also holds files from an earlier, unrelated attempt (hashed names, a
/// `ctl-*` subdirectory) — this only ever reads and writes the plain
/// `<speaker>.wav` path it owns, and never trusts a file there without
/// re-probing its rate first.
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
    let mut model = None;
    let mut via = None;
    let mut pad = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |flag: &str| args.next().with_context(|| format!("{flag} needs a path"));
        match a.as_str() {
            "--manifest" => manifest = Some(PathBuf::from(value("--manifest")?)),
            "--json" => json = Some(PathBuf::from(value("--json")?)),
            "--model" => model = Some(value("--model")?),
            "--via" => {
                via = Some(
                    value("--via")?
                        .parse::<u32>()
                        .context("--via needs a sample rate in Hz, e.g. 48000")?,
                )
            }
            "--pad" => {
                pad = Some(
                    value("--pad")?
                        .parse::<usize>()
                        .context("--pad needs a number of milliseconds, e.g. 200")?,
                )
            }
            _ => bail!(
                "usage: wer [--manifest <path>] [--json <path>] [--model <dir>] [--via <rate>] [--pad <ms>]"
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
        let samples = if let Some(rate) = via {
            let transcoded = ensure_via(&entry.wav, &entry.speaker, rate)
                .with_context(|| format!("transcoding {wav} to {rate} Hz"))?;
            let (raw, actual_rate) = resample::read_wav_any(&transcoded)
                .with_context(|| format!("reading {}", transcoded.display()))?;
            resample::to_16k(&raw, actual_rate)
                .with_context(|| format!("resampling {} to 16 kHz", transcoded.display()))?
        } else {
            features::read_wav(wav)
                .with_context(|| format!("reading {} — run scripts/fetch-fixtures", wav))?
        };
        let reference = std::fs::read_to_string(&entry.reference).with_context(|| {
            format!(
                "reading {} — run scripts/fetch-fixtures",
                entry.reference.display()
            )
        })?;
        let seconds = samples.len() as f64 / TARGET_HZ as f64;

        let mut vad = Vad::load(&vad_path)?;
        if let Some(pad_ms) = pad {
            vad.pad_ms = pad_ms;
        }
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

    if let Some(path) = json {
        let body = serde_json::to_string_pretty(&rows)?;
        std::fs::write(&path, format!("{body}\n"))
            .with_context(|| format!("writing {}", path.display()))?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}
