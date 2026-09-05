//! Word error rate over the fixture, through Ambient's own recording path.
//!
//!   cargo run --release --bin wer -- [--manifest <path>] [--json <path>]
//!
//! The point of this binary is that it is not a shortcut. It reads each
//! fixture wav, cuts it with the same `Vad::turns(.., 30)` a finished track
//! gets, and decodes the turns with `transcribe_segments` on the models
//! `ambient record` resolves — so the number it prints is the number a user
//! would get, not the number a friendlier path could get. Model resolution
//! comes from `session::model_paths`, the same call `record` makes, because a
//! second copy of that logic here would drift and score a model nobody ships.
//!
//! The fixture comes from `scripts/fetch-fixtures`; the models from
//! `./fetch-models.sh`. Missing either is a non-zero exit that says which.

use ambient::{asr::Recognizer, features, session, vad::Vad, wer};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Instant;

/// One fixture as `scripts/fetch-fixtures` writes it.
#[derive(Deserialize)]
struct Entry {
    wav: String,
    reference: String,
    speaker: String,
    seconds: f64,
}

/// One line of the table, and one element of the `--json` array. The total row
/// is the same shape with `speaker` set to `total`, so a consumer that wants
/// only the headline number does not need a second schema to find it.
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
    PathBuf::from(home)
        .join(".cache/ambient/fixtures/wer")
        .join("manifest.json")
}

fn main() -> Result<()> {
    let mut manifest = default_manifest();
    let mut json: Option<PathBuf> = None;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let need = |v: Option<&String>, flag: &str| -> Result<PathBuf> {
            match v {
                Some(p) => Ok(PathBuf::from(p)),
                None => bail!("{flag} needs a path"),
            }
        };
        match args[i].as_str() {
            "--manifest" => {
                manifest = need(args.get(i + 1), "--manifest")?;
                i += 2;
            }
            "--json" => {
                json = Some(need(args.get(i + 1), "--json")?);
                i += 2;
            }
            other => {
                bail!("unknown argument {other} — usage: wer [--manifest <path>] [--json <path>]")
            }
        }
    }

    if !manifest.is_file() {
        bail!(
            "no fixture manifest at {} — run scripts/fetch-fixtures",
            manifest.display()
        );
    }
    let entries: Vec<Entry> = serde_json::from_str(
        &std::fs::read_to_string(&manifest)
            .with_context(|| format!("reading {}", manifest.display()))?,
    )
    .with_context(|| format!("parsing {}", manifest.display()))?;

    // Resolved before anything is loaded so a missing model fails in a second
    // rather than after the first fixture has been read. Both bails name the
    // path and `./fetch-models.sh`.
    let (asr_dir, vad_path) = session::model_paths(None)?;
    let mut vad = Vad::load(
        vad_path
            .to_str()
            .with_context(|| format!("VAD path is not valid UTF-8: {}", vad_path.display()))?,
    )?;
    let mut rec = Recognizer::load(
        asr_dir
            .to_str()
            .with_context(|| format!("model path is not valid UTF-8: {}", asr_dir.display()))?,
    )?;

    let mut rows = Vec::new();
    let mut total = wer::Wer::default();
    let (mut total_seconds, mut total_decode) = (0.0f64, 0.0f64);

    for e in &entries {
        let samples = features::read_wav(&e.wav).with_context(|| format!("reading {}", e.wav))?;
        let reference = wer::normalise(
            &std::fs::read_to_string(&e.reference)
                .with_context(|| format!("reading {}", e.reference))?,
        );

        let t0 = Instant::now();
        let turns = vad.turns(&samples, 30)?;
        let segs = rec.transcribe_segments(&samples, &turns)?;
        let decode = t0.elapsed().as_secs_f64();

        let text = segs
            .into_iter()
            .map(|(_, t, _)| t)
            .collect::<Vec<_>>()
            .join(" ");
        let w = wer::score(&reference, &wer::normalise(&text));

        total.substitutions += w.substitutions;
        total.insertions += w.insertions;
        total.deletions += w.deletions;
        total.reference_words += w.reference_words;
        total_seconds += e.seconds;
        total_decode += decode;

        rows.push(Row {
            speaker: e.speaker.clone(),
            seconds: e.seconds,
            reference_words: w.reference_words,
            substitutions: w.substitutions,
            insertions: w.insertions,
            deletions: w.deletions,
            wer: w.rate(),
            decode_seconds: decode,
            realtime: e.seconds / decode,
        });
    }

    // The total is scored over the summed counts, not averaged over the rows:
    // an average would weight a 90-second speaker the same as a 520-second one
    // and move when the fixture's mix changes rather than when the pipeline does.
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
        "{:<8} {:>8} {:>7} {:>5} {:>5} {:>5} {:>7} {:>8} {:>9}",
        "speaker", "seconds", "words", "S", "I", "D", "WER", "decode", "realtime"
    );
    for (n, r) in rows.iter().enumerate() {
        if n + 1 == rows.len() {
            println!("{}", "-".repeat(68));
        }
        println!(
            "{:<8} {:>8.2} {:>7} {:>5} {:>5} {:>5} {:>6.2}% {:>7.2}s {:>8.1}x",
            r.speaker,
            r.seconds,
            r.reference_words,
            r.substitutions,
            r.insertions,
            r.deletions,
            r.wer * 100.0,
            r.decode_seconds,
            r.realtime
        );
    }

    if let Some(path) = json {
        std::fs::write(&path, serde_json::to_string_pretty(&rows)? + "\n")
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}
