//! Cost per block of a live transcriber, and the backlog that implies.
//!
//!   cargo run --release --bin asrbench -- [--wav <path>] [--block-seconds <n>]
//!                                          [--json <path>]
//!
//! Replays a recording the way a live transcriber would meet it: native-rate
//! 48 kHz audio in `--block-seconds` blocks (default 30), each resampled to
//! 16 kHz, run through VAD, its turns decoded — with a turn that ends within
//! `vad::PAD_MS` of a block edge carried forward into the next block rather
//! than cut. Every stage is timed; the timings become `bench::BlockRow`s fed
//! to `bench::simulate_queue` and `bench::repeat`, which say how far behind a
//! single worker falls with one track and with two, and again with the
//! workload doubled.

use ambient::asr::Recognizer;
use ambient::{
    bench::{self, BlockRow},
    resample, session,
    vad::{self, Vad},
};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The one field this binary reads from the `wer` fixture manifest; the rest
/// is unread the way `wer.rs` leaves them.
#[derive(Deserialize)]
struct Entry {
    wav: PathBuf,
}

fn default_manifest() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".cache/ambient/fixtures/wer/manifest.json")
}

fn cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".cache/ambient/bench")
}

/// Build (or reuse) the native-rate 48 kHz copy `ffmpeg` produces, so the
/// resampler that a live capture would run is in the timed path.
fn ensure_48k(input: &Path) -> Result<PathBuf> {
    let stem = input
        .file_stem()
        .context("input wav has no file stem")?
        .to_string_lossy()
        .into_owned();
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let dest = dir.join(format!("{stem}.48k.wav"));
    if dest.is_file() {
        return Ok(dest);
    }
    let tmp = dir.join(format!("{stem}.48k.wav.tmp"));
    let output = std::process::Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(input)
        .args(["-ar", "48000", "-ac", "1", "-f", "wav"])
        .arg(&tmp)
        .output()
        .with_context(|| format!("running ffmpeg on {}", input.display()))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!(
            "ffmpeg -ar 48000 on {} failed: {}",
            input.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::rename(&tmp, &dest)
        .with_context(|| format!("renaming {} to {}", tmp.display(), dest.display()))?;
    Ok(dest)
}

fn main() -> Result<()> {
    let mut wav = None;
    let mut block_seconds: i64 = 30;
    let mut json = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |flag: &str| args.next().with_context(|| format!("{flag} needs a value"));
        match a.as_str() {
            "--wav" => wav = Some(PathBuf::from(value("--wav")?)),
            "--block-seconds" => block_seconds = value("--block-seconds")?.parse()?,
            "--json" => json = Some(PathBuf::from(value("--json")?)),
            _ => bail!("usage: asrbench [--wav <path>] [--block-seconds <n>] [--json <path>]"),
        }
    }
    if block_seconds < 1 {
        bail!("--block-seconds must be at least 1, got {block_seconds}");
    }
    let block_seconds = block_seconds as usize;

    // Resolve the input wav before touching any model: a missing manifest and
    // no `--wav` must fail here, not after a model-load error masks it.
    let wav = match wav {
        Some(w) => w,
        None => {
            let manifest = default_manifest();
            let raw = std::fs::read_to_string(&manifest).with_context(|| {
                format!(
                    "no fixture manifest at {} and no --wav given — pass --wav <path>",
                    manifest.display()
                )
            })?;
            let entries: Vec<Entry> = serde_json::from_str(&raw)
                .with_context(|| format!("parsing {}", manifest.display()))?;
            let first = entries.first().with_context(|| {
                format!(
                    "{} lists no fixtures — pass --wav <path>",
                    manifest.display()
                )
            })?;
            first.wav.clone()
        }
    };

    let (asr_dir, vad_path) = session::model_paths(None)
        .context("the models this run needs are missing — run ./fetch-models.sh")?;
    let asr_dir = asr_dir
        .to_str()
        .context("ASR model path is not valid UTF-8")?
        .to_string();
    let vad_path = vad_path
        .to_str()
        .context("VAD model path is not valid UTF-8")?
        .to_string();

    let t0 = Instant::now();
    let mut vad = Vad::load(&vad_path)?;
    let mut rec = Recognizer::load(&asr_dir)?;
    let model_load_s = t0.elapsed().as_secs_f64();

    let src = ensure_48k(&wav)?;
    let (native, rate) = resample::read_wav_any(&src)?;
    let rate_usize = rate as usize;
    let block_len = block_seconds * rate_usize;
    let pad_samples = vad::PAD_MS * resample::TARGET_HZ as usize / 1000;

    let mut rows: Vec<BlockRow> = Vec::new();
    let mut tail: Vec<f32> = Vec::new();
    let mut cursor = 0usize;
    let mut end_s = 0.0f64;

    while cursor < native.len() {
        let chunk_end = (cursor + block_len).min(native.len());
        let new_chunk = &native[cursor..chunk_end];
        let is_last = chunk_end == native.len();

        let mut block_native = std::mem::take(&mut tail);
        block_native.extend_from_slice(new_chunk);

        let audio_s = new_chunk.len() as f64 / rate as f64;
        end_s += audio_s;

        let t0 = Instant::now();
        let block_16k = resample::to_16k(&block_native, rate)?;
        let turns = vad.turns(&block_16k, block_seconds)?;
        let prep_s = t0.elapsed().as_secs_f64();

        let (decoded, held): (Vec<vad::Segment>, Vec<vad::Segment>) = if is_last {
            (turns, Vec::new())
        } else {
            let cutoff = block_16k.len().saturating_sub(pad_samples);
            turns.into_iter().partition(|s| s.end < cutoff)
        };

        let t1 = Instant::now();
        let _ = rec.transcribe_segments(&block_16k, &decoded)?;
        let decode_s = t1.elapsed().as_secs_f64();

        rows.push(BlockRow {
            index: rows.len(),
            audio_s,
            end_s,
            prep_s,
            decode_s,
            turns: decoded.len(),
        });

        if let Some(held_start) = held.iter().map(|s| s.start).min() {
            let native_start = (held_start * rate_usize / 16_000).min(block_native.len());
            tail = block_native[native_start..].to_vec();
        }

        cursor = chunk_end;
    }

    println!(
        "{:>5} {:>9} {:>9} {:>8} {:>9} {:>6} {:>8}",
        "index", "audio_s", "end_s", "prep_s", "decode_s", "turns", "rtf"
    );
    for r in &rows {
        let rtf = bench::rtf(r.prep_s + r.decode_s, r.audio_s);
        println!(
            "{:>5} {:>9.2} {:>9.2} {:>8.3} {:>9.3} {:>6} {:>8}",
            r.index,
            r.audio_s,
            r.end_s,
            r.prep_s,
            r.decode_s,
            r.turns,
            rtf.map(|v| format!("{v:.3}"))
                .unwrap_or_else(|| "None".into())
        );
    }

    let total_audio: f64 = rows.iter().map(|r| r.audio_s).sum();
    let total_prep: f64 = rows.iter().map(|r| r.prep_s).sum();
    let total_decode: f64 = rows.iter().map(|r| r.decode_s).sum();
    let total_rtf = bench::rtf(total_prep + total_decode, total_audio);
    let max_block_work_s = rows
        .iter()
        .map(|r| r.prep_s + r.decode_s)
        .fold(0.0f64, f64::max);

    println!(
        "\nblocks={} audio_s={:.2} prep_s={:.3} decode_s={:.3} rtf={} max_block_work_s={:.3} model_load_s={:.3}",
        rows.len(),
        total_audio,
        total_prep,
        total_decode,
        total_rtf.map(|v| format!("{v:.3}")).unwrap_or_else(|| "None".into()),
        max_block_work_s,
        model_load_s,
    );

    let doubled = bench::repeat(&rows, 2);
    let lag_1 = bench::simulate_queue(&rows, 1);
    let lag_2 = bench::simulate_queue(&rows, 2);
    let lag_x2_1 = bench::simulate_queue(&doubled, 1);
    let lag_x2_2 = bench::simulate_queue(&doubled, 2);

    println!(
        "tracks=1        max_lag_s={:.3} final_lag_s={:.3}",
        lag_1.max_lag_s, lag_1.final_lag_s
    );
    println!(
        "tracks=1 (x2)   max_lag_s={:.3} final_lag_s={:.3}",
        lag_x2_1.max_lag_s, lag_x2_1.final_lag_s
    );
    println!(
        "tracks=2        max_lag_s={:.3} final_lag_s={:.3}",
        lag_2.max_lag_s, lag_2.final_lag_s
    );
    println!(
        "tracks=2 (x2)   max_lag_s={:.3} final_lag_s={:.3}",
        lag_x2_2.max_lag_s, lag_x2_2.final_lag_s
    );

    if let Some(path) = json {
        let blocks_json: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                let rtf = bench::rtf(r.prep_s + r.decode_s, r.audio_s);
                serde_json::json!({
                    "index": r.index,
                    "audio_s": r.audio_s,
                    "end_s": r.end_s,
                    "prep_s": r.prep_s,
                    "decode_s": r.decode_s,
                    "turns": r.turns,
                    "rtf": rtf,
                })
            })
            .collect();
        let total_json = serde_json::json!({
            "blocks": rows.len(),
            "audio_s": total_audio,
            "prep_s": total_prep,
            "decode_s": total_decode,
            "rtf": total_rtf,
            "max_block_work_s": max_block_work_s,
            "model_load_s": model_load_s,
        });
        let body = serde_json::json!({
            "blocks": blocks_json,
            "total": total_json,
            "lag": {
                "1": {"max_lag_s": lag_1.max_lag_s, "final_lag_s": lag_1.final_lag_s},
                "2": {"max_lag_s": lag_2.max_lag_s, "final_lag_s": lag_2.final_lag_s},
            },
            "lag_x2": {
                "1": {"max_lag_s": lag_x2_1.max_lag_s, "final_lag_s": lag_x2_1.final_lag_s},
                "2": {"max_lag_s": lag_x2_2.max_lag_s, "final_lag_s": lag_x2_2.final_lag_s},
            },
        });
        std::fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&body)?))
            .with_context(|| format!("writing {}", path.display()))?;
        eprintln!("wrote {}", path.display());
    }

    Ok(())
}
