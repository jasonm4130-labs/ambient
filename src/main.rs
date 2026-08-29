//! ambient — local-first capture, transcription and attribution.

use anyhow::{bail, Result};

const USAGE: &str = "\
ambient — local-first ambient capture

USAGE
  ambient probe                        check this machine is viable
  ambient transcribe <model-dir> <a.wav>   transcribe a 16 kHz wav
  ambient tap <out.wav> <secs> [bundle-id...]   record system audio, no bot
";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("probe") => {
            ambient::probe::run()?;
            ambient::probe::probe_ort();
            Ok(())
        }
        Some("transcribe") => {
            let model = args.next().unwrap_or_default();
            let wav = args.next().unwrap_or_default();
            if model.is_empty() || wav.is_empty() {
                bail!("{USAGE}");
            }
            let samples = ambient::features::read_wav(&wav)?;
            let secs = samples.len() as f64 / 16_000.0;
            let t = std::time::Instant::now();
            let mut rec = ambient::asr::Recognizer::load(&model)?;
            let text = rec.transcribe_long(&samples)?;
            let el = t.elapsed().as_secs_f64();
            eprintln!("{secs:.1}s audio in {el:.2}s ({:.0}x realtime)", secs / el);
            println!("{text}");
            Ok(())
        }
        Some("tap") => {
            let out = args.next().unwrap_or_default();
            let secs: u64 = args.next().unwrap_or_default().parse().unwrap_or(10);
            let bundles: Vec<String> = args.collect();
            if out.is_empty() {
                bail!("{USAGE}");
            }
            if bundles.is_empty() {
                eprintln!("tapping ALL system audio for {secs}s");
            } else {
                eprintln!("tapping {} for {secs}s", bundles.join(", "));
            }

            let tap = ambient::capture::ProcessTap::start(&bundles, 60, true)?;
            eprintln!(
                "running: {} Hz, {} ch total ({} mic + {} call)",
                tap.sample_rate as u32,
                tap.channels,
                tap.mic_channels,
                tap.channels - tap.mic_channels
            );

            let mut cursor = 0usize;
            let mut all: Vec<f32> = Vec::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
            while std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(200));
                let (chunk, next) = tap.ring.drain_from(cursor);
                cursor = next;
                all.extend_from_slice(&chunk);
            }

            let spec = hound::WavSpec {
                channels: tap.channels as u16,
                sample_rate: tap.sample_rate as u32,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::create(&out, spec)?;
            // Per-channel peaks, so a silent track is obvious immediately.
            let ch = tap.channels as usize;
            let mut peaks = vec![0.0f32; ch.max(1)];
            for (i, s) in all.iter().enumerate() {
                let c = i % ch.max(1);
                peaks[c] = peaks[c].max(s.abs());
                w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
            }
            w.finalize()?;
            let peak = peaks.iter().cloned().fold(0.0f32, f32::max);
            for (i, p) in peaks.iter().enumerate() {
                let label = if (i as u32) < tap.mic_channels { "mic " } else { "call" };
                eprintln!("  ch{i} {label} peak {p:.3}");
            }
            eprintln!(
                "wrote {out}: {} samples, {:.1}s, peak {:.3}",
                all.len(),
                all.len() as f64 / (tap.sample_rate * tap.channels as f64),
                peak
            );
            if peak < 1e-4 {
                eprintln!("WARNING: silence captured — was anything actually playing?");
            }
            Ok(())
        }
        _ => {
            print!("{USAGE}");
            Ok(())
        }
    }
}
