//! Word error rate for Ambient's own transcription path.
//!
//!   cargo run --release --bin wer -- [--manifest <path>] [--json <path>]
//!                                     [--model <dir>] [--via <hz>]
//!                                     [--via-ffmpeg]
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
//! `--via <hz>` puts the fixture through `resample::to_16k` first, by
//! upsampling each wav to that rate with `ffmpeg` and resampling it back. The
//! fixture is native 16 kHz, so without this the resampler every real
//! recording goes through — Core Audio delivers 48 kHz — is never measured.
//!
//! `--via-ffmpeg` is the control for that measurement: the identical round
//! trip with `ffmpeg` on the return leg too, so `resample::to_16k` never runs.
//! A `--via` run on its own cannot tell our resampler apart from ffmpeg's
//! upsample and the `pcm_s16le` requantisation it shares; the difference
//! between the two runs is what our code costs.

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

/// True when `dst` was built from the current `src`. An mtime comparison, not
/// a hash: the fixture is rewritten only by `scripts/fetch-fixtures`, and a
/// cache that never invalidated would let `--force`, or another manifest
/// reusing a LibriSpeech speaker id, score last night's audio against
/// tonight's references with nothing on the table saying so.
fn current(src: &Path, dst: &Path) -> bool {
    let stamp = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (stamp(src), stamp(dst)) {
        (Some(src), Some(dst)) => dst >= src,
        _ => false,
    }
}

/// The cache path for one leg of a `--via` round trip, under
/// `~/.cache/ambient/fixtures/via/<leg>/<speaker>.wav` — a sibling tree, not
/// the fixture directory, so `scripts/fetch-fixtures` owns its own output.
fn via_path(leg: &str, speaker: &str) -> Result<PathBuf> {
    let dir = cache_root().join("fixtures/via").join(leg);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir.join(format!("{speaker}.wav")))
}

/// `src` resampled to `hz` by ffmpeg at `dst`. Written once and reused while
/// it stays current: the point of measuring is what happens to the samples
/// after, and re-running ffmpeg every time would add a fixed cost to every run.
fn ffmpeg_resample(src: &Path, dst: &Path, hz: u32) -> Result<()> {
    if current(src, dst) {
        return Ok(());
    }
    let out = Command::new("ffmpeg")
        .args(["-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(src)
        .args(["-ac", "1", "-c:a", "pcm_s16le", "-ar"])
        .arg(hz.to_string())
        .arg(dst)
        .output()
        .context("running ffmpeg — --via needs it on PATH (brew install ffmpeg)")?;
    if !out.status.success() {
        let _ = std::fs::remove_file(dst);
        bail!(
            "ffmpeg could not resample {} to {hz} Hz: {}",
            src.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// A cached wav read back at the rate it was written at.
fn read_at(path: &Path, hz: u32) -> Result<Vec<f32>> {
    let (samples, rate) = resample::read_wav_any(path)?;
    if rate != hz {
        bail!(
            "{} is {rate} Hz, not the {hz} Hz it was written at",
            path.display()
        );
    }
    Ok(samples)
}

/// The fixture's samples at 16 kHz: read straight, taken the long way round
/// through `hz` and back through `resample::to_16k`, or — with `control` —
/// taken that same way round with ffmpeg on both legs, which leaves
/// `resample::to_16k` out of the path entirely. The control run is what says
/// how much of a `--via` run's damage is ours and how much is the ffmpeg
/// upsample and s16 requantisation both runs share.
fn samples_for(wav: &str, speaker: &str, via: Option<u32>, control: bool) -> Result<Vec<f32>> {
    let Some(hz) = via else {
        return features::read_wav(wav)
            .with_context(|| format!("reading {wav} — run scripts/fetch-fixtures"));
    };
    let up = via_path(&hz.to_string(), speaker)?;
    ffmpeg_resample(Path::new(wav), &up, hz)?;
    if control {
        let back = via_path(&format!("{hz}-ffmpeg-16k"), speaker)?;
        ffmpeg_resample(&up, &back, TARGET_HZ)?;
        return read_at(&back, TARGET_HZ);
    }
    let samples = read_at(&up, hz)?;
    resample::to_16k(&samples, hz)
}

fn main() -> Result<()> {
    let mut manifest = None;
    let mut json = None;
    let mut model = None;
    let mut via: Option<u32> = None;
    let mut control = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |flag: &str| args.next().with_context(|| format!("{flag} needs a value"));
        match a.as_str() {
            "--manifest" => manifest = Some(PathBuf::from(value("--manifest")?)),
            "--json" => json = Some(PathBuf::from(value("--json")?)),
            "--model" => model = Some(value("--model")?),
            "--via" => {
                let hz = value("--via")?;
                via = Some(
                    hz.parse()
                        .with_context(|| format!("--via wants a sample rate in hz, not {hz:?}"))?,
                );
            }
            "--via-ffmpeg" => control = true,
            _ => bail!(
                "usage: wer [--manifest <path>] [--json <path>] [--model <dir>] \
                 [--via <hz>] [--via-ffmpeg]"
            ),
        }
    }
    if control && via.is_none() {
        bail!("--via-ffmpeg is the control for a --via run, so it needs --via <hz> too");
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
        let samples = samples_for(wav, &entry.speaker, via, control)?;
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
