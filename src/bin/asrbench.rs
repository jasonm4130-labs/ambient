//! What live transcription would cost, block by block, and how far behind it
//! would fall.
//!
//!   cargo run --release --bin asrbench -- [--wav <path>] [--block-seconds <n>] [--json <path>]
//!
//! A live transcriber does not meet a recording the way `wer` does. It gets
//! native-rate audio a block at a time, and pays for a resample and a VAD pass
//! on every block before it decodes anything. So this replays a wav that way:
//! upsampled to the capture's 48 kHz once, cut into `--block-seconds` blocks,
//! each block resampled, segmented and decoded with the models `ambient record`
//! resolves, every stage timed. The timings then feed `bench::simulate_queue`,
//! which says how far behind one worker gets with one track and with two.
//!
//! Two details keep the replay honest. Speech still running at a block's edge
//! is not decoded there — its samples are carried into the next block — so a
//! 40 s utterance splits at an interior quiet frame the way the whole-track
//! path splits it, not at whatever second the block boundary lands on. And
//! `Vad::turns` resets its recurrent state on every call (`src/vad.rs:131`),
//! which a per-block pass pays once per block; that cost is left in, because a
//! live pass built on today's `Vad` would pay it too.

use ambient::bench::{repeat, rtf, simulate_queue, BlockRow};
use ambient::{asr::Recognizer, resample, session, vad::Vad};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// What the capture layer hands back on this Mac. The input is upsampled to it
/// so the resampler is inside the timed path rather than assumed away.
const NATIVE_HZ: u32 = 48_000;

/// `vad::PAD_MS`, which is private to that module: the margin the VAD keeps
/// either side of a turn. A turn ending within it of the block's end is one
/// the VAD had no silence to close on, so it is speech in progress.
const PAD_MS: usize = 200;

/// One fixture entry; only the wav is read, and only the first entry is used.
#[derive(Deserialize)]
struct Fixture {
    wav: PathBuf,
}

fn cache_dir(sub: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".cache/ambient").join(sub)
}

/// The first fixture `scripts/fetch-fixtures` built, so the default input is a
/// property of the corpus rather than a path someone typed.
fn default_wav() -> Result<PathBuf> {
    let manifest = cache_dir("fixtures/wer").join("manifest.json");
    let raw = std::fs::read_to_string(&manifest).with_context(|| {
        format!(
            "no fixture manifest at {} — run scripts/fetch-fixtures, or pass --wav <path>",
            manifest.display()
        )
    })?;
    let entries: Vec<Fixture> =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", manifest.display()))?;
    match entries.into_iter().next() {
        Some(f) => Ok(f.wav),
        None => bail!("{} lists no fixtures", manifest.display()),
    }
}

/// A 48 kHz copy of `wav`, made once and cached. ffmpeg does the upsampling:
/// the point is to give the benchmark native-rate input, not to measure the
/// upsampler.
fn upsampled(wav: &Path) -> Result<PathBuf> {
    let dir = cache_dir("bench");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let stem = wav
        .file_stem()
        .and_then(|s| s.to_str())
        .with_context(|| format!("{} has no usable file name", wav.display()))?;
    let out = dir.join(format!("{stem}.48k.wav"));
    if out.is_file() {
        return Ok(out);
    }
    let status = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(wav)
        .args(["-ar", &NATIVE_HZ.to_string(), "-ac", "1"])
        .arg(&out)
        .status()
        .context("running ffmpeg — install it with `brew install ffmpeg`")?;
    if !status.success() {
        bail!("ffmpeg failed to write {}", out.display());
    }
    Ok(out)
}

fn show(v: Option<f64>) -> String {
    v.map_or_else(|| "—".into(), |v| format!("{v:.3}"))
}

fn main() -> Result<()> {
    let (mut wav, mut json) = (None, None);
    let mut block_seconds: usize = 30;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |flag: &str| args.next().with_context(|| format!("{flag} needs a value"));
        match a.as_str() {
            "--wav" => wav = Some(PathBuf::from(value("--wav")?)),
            "--json" => json = Some(PathBuf::from(value("--json")?)),
            "--block-seconds" => {
                let raw = value("--block-seconds")?;
                block_seconds = raw
                    .parse()
                    .with_context(|| format!("--block-seconds {raw} is not a number"))?;
                if block_seconds < 1 {
                    bail!("--block-seconds must be at least 1");
                }
            }
            _ => bail!("usage: asrbench [--wav <path>] [--block-seconds <n>] [--json <path>]"),
        }
    }
    let wav = match wav {
        Some(w) => w,
        None => default_wav()?,
    };
    let native = upsampled(&wav)?;
    let (samples, rate) = resample::read_wav_any(&native)?;

    // The one resolution `record` uses: a benchmark pointed at another model
    // measures a pipeline no user has.
    let (asr_dir, vad_path) = session::model_paths(None)
        .context("the models `ambient record` uses are missing — run ./fetch-models.sh")?;
    let asr_dir = asr_dir
        .to_str()
        .context("ASR path is not UTF-8")?
        .to_string();
    let vad_path = vad_path
        .to_str()
        .context("VAD path is not UTF-8")?
        .to_string();
    let load = Instant::now();
    let mut vad = Vad::load(&vad_path)?;
    let mut rec = Recognizer::load(&asr_dir)?;
    let model_load_s = load.elapsed().as_secs_f64();

    println!(
        "input: {} at {rate} Hz ({:.1}s), {block_seconds}s blocks",
        native.display(),
        samples.len() as f64 / rate as f64
    );

    let block_len = block_seconds * rate as usize;
    let pad16 = PAD_MS * resample::TARGET_HZ as usize / 1000;
    let mut rows: Vec<BlockRow> = Vec::new();
    let mut carry: Vec<f32> = Vec::new();
    let mut consumed = 0usize;

    for (index, chunk) in samples.chunks(block_len).enumerate() {
        let mut block = std::mem::take(&mut carry);
        block.extend_from_slice(chunk);
        consumed += chunk.len();
        let last = consumed >= samples.len();

        let prep = Instant::now();
        let block16 = resample::to_16k(&block, rate)?;
        let turns = vad.turns(&block16, block_seconds)?;
        let prep_s = prep.elapsed().as_secs_f64();

        // Speech that runs to the block's edge is unfinished: hold it back and
        // let the next block, which has the rest of it, decide where it ends.
        let held = (!last)
            .then(|| turns.last())
            .flatten()
            .filter(|t| block16.len().saturating_sub(t.end) <= pad16)
            .copied();
        let decoded = &turns[..turns.len() - usize::from(held.is_some())];

        let decode = Instant::now();
        rec.transcribe_segments(&block16, decoded)?;
        let decode_s = decode.elapsed().as_secs_f64();

        if let Some(t) = held {
            let from = (t.start * rate as usize / resample::TARGET_HZ as usize).min(block.len());
            carry = block[from..].to_vec();
        }

        rows.push(BlockRow {
            index,
            audio_s: chunk.len() as f64 / rate as f64,
            end_s: consumed as f64 / rate as f64,
            prep_s,
            decode_s,
            turns: decoded.len(),
        });
    }

    println!(
        "\n{:>5} {:>8} {:>8} {:>8} {:>9} {:>6} {:>7}",
        "block", "audio_s", "end_s", "prep_s", "decode_s", "turns", "rtf"
    );
    for r in &rows {
        println!(
            "{:>5} {:>8.2} {:>8.2} {:>8.2} {:>9.2} {:>6} {:>7}",
            r.index,
            r.audio_s,
            r.end_s,
            r.prep_s,
            r.decode_s,
            r.turns,
            show(rtf(r.work_s(), r.audio_s))
        );
    }

    let audio_s: f64 = rows.iter().map(|r| r.audio_s).sum();
    let prep_s: f64 = rows.iter().map(|r| r.prep_s).sum();
    let decode_s: f64 = rows.iter().map(|r| r.decode_s).sum();
    let max_block_work_s = rows.iter().map(BlockRow::work_s).fold(0.0, f64::max);
    let total_rtf = rtf(prep_s + decode_s, audio_s);
    println!(
        "\ntotal {} blocks, audio {audio_s:.2}s, prep {prep_s:.2}s, decode {decode_s:.2}s, \
         rtf {}, max block work {max_block_work_s:.2}s, model load {model_load_s:.2}s",
        rows.len(),
        show(total_rtf)
    );

    // Twice the workload is the test for a bounded backlog: a queue that keeps
    // up lags the same on a track of any length, one that does not adds to it
    // block after block.
    let doubled = repeat(&rows, 2);
    println!(
        "\n{:<9} {:>6} {:>10} {:>12}  verdict",
        "workload", "tracks", "max_lag_s", "final_lag_s"
    );
    let mut lags = Vec::new();
    for (label, set) in [("1x", &rows), ("2x", &doubled)] {
        for tracks in [1usize, 2] {
            let lag = simulate_queue(set, tracks);
            lags.push(lag);
            let verdict = match label {
                "1x" => String::new(),
                _ => {
                    let single = simulate_queue(&rows, tracks).max_lag_s;
                    if (lag.max_lag_s - single).abs() <= 1.0 {
                        "bounded — the same lag on a track twice as long".into()
                    } else {
                        format!("accumulating — {:.1}x the 1x lag", lag.max_lag_s / single)
                    }
                }
            };
            println!(
                "{label:<9} {tracks:>6} {:>10.2} {:>12.2}  {verdict}",
                lag.max_lag_s, lag.final_lag_s
            );
        }
    }

    if let Some(path) = json {
        let body = serde_json::json!({
            "blocks": rows,
            "total": {
                "blocks": rows.len(),
                "audio_s": audio_s,
                "prep_s": prep_s,
                "decode_s": decode_s,
                "rtf": total_rtf,
                "max_block_work_s": max_block_work_s,
                "model_load_s": model_load_s,
            },
            "lag": { "1": lags[0], "2": lags[1] },
            "lag_x2": { "1": lags[2], "2": lags[3] },
        });
        let body = serde_json::to_string_pretty(&body)?;
        std::fs::write(&path, format!("{body}\n"))
            .with_context(|| format!("writing {}", path.display()))?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}
