//! ambient — local-first capture, transcription and attribution.

use anyhow::{bail, Result};

const USAGE: &str = "\
ambient — local-first ambient capture

USAGE
  ambient probe                        check this machine is viable
  ambient transcribe <model-dir> <a.wav>   transcribe a 16 kHz wav
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
        _ => {
            print!("{USAGE}");
            Ok(())
        }
    }
}
