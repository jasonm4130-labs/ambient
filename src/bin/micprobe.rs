//! The microphone on its own: no tap, no aggregate, nothing else running.
//! Isolates whether the input-device IOProc blocks by itself or only alongside
//! the tap's aggregate.
fn main() -> anyhow::Result<()> {
    eprintln!("starting mic-only capture");
    let tap = ambient::capture::ProcessTap::start_mic_only()?;
    eprintln!("running: {} Hz x {} ch", tap.mic_rate, tap.mic_channels);
    let mut d = ambient::capture::Drain::default();
    let mut peak = 0.0f32;
    let mut n = 0usize;
    for _ in 0..15 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let (room, _) = tap.drain(&mut d);
        n += room.len();
        for s in &room {
            peak = peak.max(s.abs());
        }
    }
    println!("mic-only: {n} samples, peak {peak:.3}");
    Ok(())
}
