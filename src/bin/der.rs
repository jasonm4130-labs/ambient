//! Diarization error rate for Ambient's own speaker-separation path.
//!
//!   cargo run --release --bin der -- [--manifest <path>] [--collar <f64>]
//!                                    [--json <path>]
//!
//! Scores the AMI fixture `scripts/fetch-fixtures` builds by running what the
//! app runs for a finished session: `ambient-engine diarize` over the 16 kHz
//! track, located the way the app locates it (`$AMBIENT_ENGINE`, else beside
//! this binary) with the same models root. A harness with its own diariser
//! would measure a pipeline no user has. What is scored is the helper's turns,
//! before the app folds a phantom speaker's words into a neighbour.
//!
//! `--collar` exists so a number here can be compared with a published one,
//! which is quoted at 0.25 s far more often than at 0.

use ambient::{
    der::{parse_rttm, score, Der, Turn},
    engine::{SpeakersFile, Stage},
    pipeline::Engine,
    resample::{read_wav_any, to_16k, TARGET_HZ},
    session::write_wav_16k,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Instant;

/// One fixture as `scripts/fetch-fixtures` writes it. `seconds` is in the
/// manifest too and deliberately unread: the duration below comes from the
/// audio actually decoded, so a stale manifest cannot quietly change a rate.
#[derive(Deserialize)]
struct Entry {
    wav: PathBuf,
    rttm: PathBuf,
    meeting: String,
}

/// One printed row, and one element of the `--json` array. The last row is the
/// total, with `meeting` reading `total`; its DER is computed over the summed
/// error seconds, not averaged over the rows, so a meeting with little speech
/// in it cannot outvote a talkative one.
#[derive(Serialize)]
struct Row {
    meeting: String,
    seconds: f64,
    reference_speakers: usize,
    hypothesis_speakers: usize,
    missed_s: f64,
    false_alarm_s: f64,
    confusion_s: f64,
    der: f64,
    run_s: f64,
}

fn default_manifest() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".cache/ambient/fixtures/der/manifest.json")
}

/// How many distinct speakers a turn list names.
fn speakers(turns: &[ambient::der::Turn]) -> usize {
    let mut names: Vec<&str> = turns.iter().map(|t| t.speaker.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    names.len()
}

fn main() -> Result<()> {
    let mut manifest = None;
    let mut json = None;
    let mut collar = 0.25f64;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |flag: &str| args.next().with_context(|| format!("{flag} needs a value"));
        match a.as_str() {
            "--manifest" => manifest = Some(PathBuf::from(value("--manifest")?)),
            "--json" => json = Some(PathBuf::from(value("--json")?)),
            "--collar" => {
                let raw = value("--collar")?;
                collar = raw
                    .parse()
                    .with_context(|| format!("--collar {raw:?} is not a number"))?;
            }
            _ => bail!("usage: der [--manifest <path>] [--collar <f64>] [--json <path>]"),
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
    let scratch = std::env::temp_dir().join(format!("ambient-der-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;

    let mut rows: Vec<Row> = Vec::new();
    let mut total = Der::default();
    let (mut total_seconds, mut total_run) = (0.0f64, 0.0f64);

    for entry in &entries {
        let (audio, rate) = read_wav_any(&entry.wav).with_context(|| {
            format!(
                "reading {} — run scripts/fetch-fixtures",
                entry.wav.display()
            )
        })?;
        let samples = to_16k(&audio, rate)?;
        let seconds = samples.len() as f64 / f64::from(TARGET_HZ);

        let text = std::fs::read_to_string(&entry.rttm).with_context(|| {
            format!(
                "reading {} — run scripts/fetch-fixtures",
                entry.rttm.display()
            )
        })?;
        let reference =
            parse_rttm(&text).with_context(|| format!("parsing {}", entry.rttm.display()))?;
        // `score` would reject this too, but only as "empty reference" with no
        // clue which of three files is the empty one.
        if reference.is_empty() {
            bail!(
                "{} holds no turns — nothing to score against; run scripts/fetch-fixtures --force",
                entry.rttm.display()
            );
        }

        // The helper reads a 16 kHz mono file, which is what the app hands it.
        let wav = scratch.join("track.wav");
        write_wav_16k(&wav, &samples)?;
        // The clock includes the helper loading its model: in the app that
        // cost is paid once per track, the same as here.
        let t0 = Instant::now();
        let speakers_file: SpeakersFile =
            engine.once(Stage::Diarize, &wav, &scratch.join("speakers.json"))?;
        let run = t0.elapsed().as_secs_f64();
        let hypothesis: Vec<Turn> = speakers_file
            .turns
            .iter()
            .map(|t| Turn {
                start_s: t.start,
                end_s: t.end,
                speaker: t.speaker.to_string(),
            })
            .collect();

        let scored = score(&reference, &hypothesis, collar)
            .with_context(|| format!("scoring {}", entry.meeting))?;

        total.missed_s += scored.missed_s;
        total.false_alarm_s += scored.false_alarm_s;
        total.confusion_s += scored.confusion_s;
        total.reference_s += scored.reference_s;
        total_seconds += seconds;
        total_run += run;

        rows.push(Row {
            meeting: entry.meeting.clone(),
            seconds,
            reference_speakers: speakers(&reference),
            hypothesis_speakers: speakers(&hypothesis),
            missed_s: scored.missed_s,
            false_alarm_s: scored.false_alarm_s,
            confusion_s: scored.confusion_s,
            der: scored.rate(),
            run_s: run,
        });
    }

    // Speaker counts sum across meetings rather than being deduplicated:
    // speaker 0 of one meeting is not speaker 0 of the next.
    let reference_speakers = rows.iter().map(|r| r.reference_speakers).sum();
    let hypothesis_speakers = rows.iter().map(|r| r.hypothesis_speakers).sum();
    rows.push(Row {
        meeting: "total".into(),
        seconds: total_seconds,
        reference_speakers,
        hypothesis_speakers,
        missed_s: total.missed_s,
        false_alarm_s: total.false_alarm_s,
        confusion_s: total.confusion_s,
        der: total.rate(),
        run_s: total_run,
    });

    println!(
        "{:<9} {:>9} {:>6} {:>6} {:>9} {:>9} {:>9} {:>8} {:>8}",
        "meeting", "seconds", "ref", "hyp", "missed", "false", "confusion", "DER", "run"
    );
    for r in &rows {
        println!(
            "{:<9} {:>9.2} {:>6} {:>6} {:>9.2} {:>9.2} {:>9.2} {:>8.4} {:>7.2}s",
            r.meeting,
            r.seconds,
            r.reference_speakers,
            r.hypothesis_speakers,
            r.missed_s,
            r.false_alarm_s,
            r.confusion_s,
            r.der,
            r.run_s
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
