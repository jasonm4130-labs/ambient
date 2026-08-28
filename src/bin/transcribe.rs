use ambient::{asr, features};
use anyhow::Result;
use std::time::Instant;

fn main() -> Result<()> {
    let mut a = std::env::args().skip(1);
    let model = a.next().expect("usage: transcribe <model-dir> <audio.wav>");
    let wav = a.next().expect("usage: transcribe <model-dir> <audio.wav>");

    let samples = features::read_wav(&wav)?;
    let secs = samples.len() as f64 / 16_000.0;

    let t0 = Instant::now();
    let mut rec = asr::Recognizer::load(&model)?;
    let load = t0.elapsed();

    let t1 = Instant::now();
    let text = rec.transcribe_long(&samples)?;
    let run = t1.elapsed();

    println!("audio    {secs:.1}s");
    println!("load     {:.2}s", load.as_secs_f64());
    println!("decode   {:.2}s ({:.0}x realtime)", run.as_secs_f64(), secs / run.as_secs_f64());
    println!("\n{text}\n");
    Ok(())
}
