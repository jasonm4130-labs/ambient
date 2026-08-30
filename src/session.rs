//! Recording to, and reading back, an on-disk session.
//!
//! The format is deliberately append-only. `raw.jsonl` is what the recogniser
//! heard and is never rewritten; `edits.jsonl` carries everything applied on top
//! — repairs, speaker names, and the reverts of both. Folding the second over
//! the first gives the tidied view, and ignoring it gives the verbatim one, so
//! "what did it actually hear" always has an answer that does not depend on
//! trusting the layer above it.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use crate::capture::ProcessTap;
use crate::resample;

/// Which of the two captured tracks a line of speech came from. This is the
/// only speaker attribution available before diarization, and for a one-to-one
/// call it is already complete: the room track is you, the call track is them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Track {
    Room,
    Call,
}

impl Track {
    pub fn as_str(self) -> &'static str {
        match self {
            Track::Room => "room",
            Track::Call => "call",
        }
    }
}

/// One VAD segment as the recogniser transcribed it. Immutable once written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawRecord {
    pub track: Track,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// Mean probability of the tokens the decoder emitted for this line.
    /// Parakeet invents fluent sentences from an empty room and no threshold
    /// upstream can catch that, so the number is carried here for a cleanup
    /// pass that can read the words. `default` keeps sessions recorded before
    /// this field loadable.
    #[serde(default)]
    pub confidence: f32,
}

/// What an edit points at. VAD segments never overlap within a track, so the
/// pair is a stable identity without needing ids in `raw.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub track: Track,
    pub start_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Edit {
    /// Replace one substring within a segment's text.
    Repair {
        target: Target,
        from: String,
        to: String,
        by: String,
        at: String,
    },
    /// Name the speaker of a segment.
    Speaker {
        target: Target,
        name: String,
        by: String,
        at: String,
    },
    /// Undo an earlier edit, addressed by its zero-based line in `edits.jsonl`.
    /// Undoing appends; it never deletes.
    Revert {
        target_seq: usize,
        by: String,
        at: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub name: Option<String>,
    pub started_at: String,
    pub ended_at: String,
    pub duration_s: f64,
    pub device_hz: u32,
    /// The microphone has its own device and so its own rate.
    #[serde(default)]
    pub mic_hz: u32,
    pub channels: u32,
    pub mic_channels: u32,
    pub apps: Vec<String>,
    pub model: String,
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    // Storing to an atomic is async-signal-safe; anything else here is not.
    STOP.store(true, Ordering::SeqCst);
}

/// Where sessions live. Visible on purpose — Time Machine picks it up and the
/// user can open the folder without being told a hidden path.
pub fn home() -> PathBuf {
    // The environment wins over the stored setting so a test harness can point
    // somewhere harmless without editing the user's config.
    if let Ok(p) = std::env::var("AMBIENT_HOME") {
        return PathBuf::from(p);
    }
    if let Some(p) = crate::config::Config::load().sessions_dir {
        return p;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Documents").join("Ambient")
}

/// Locate the `models/` directory. Launched from a terminal the working
/// directory is the repo, but launched as a bundle via `open -a` it is `/` —
/// and the bundle is the only launch that has the audio-capture grant, so
/// relative paths alone would break the one path that actually works.
pub fn models_root() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("AMBIENT_MODELS") {
        return Ok(PathBuf::from(p));
    }
    let mut candidates = vec![PathBuf::from("models")];
    if let Ok(exe) = std::env::current_exe() {
        // A release bundle ships its models in Contents/Resources/models, which
        // is a *sibling* of Contents/MacOS — the parent walk below climbs past
        // it and would never look inside. This is the only candidate that makes
        // a downloaded .app work with no repo checkout anywhere on the machine.
        if let Some(macos) = exe.parent() {
            candidates.push(macos.join("../Resources/models"));
        }
        let mut dir = exe.parent().map(Path::to_path_buf);
        for _ in 0..5 {
            match dir {
                Some(d) => {
                    candidates.push(d.join("models"));
                    dir = d.parent().map(Path::to_path_buf);
                }
                None => break,
            }
        }
    }
    for c in &candidates {
        if c.is_dir() {
            return Ok(c.clone());
        }
    }
    bail!(
        "no models/ directory found (looked in {}). Run ./fetch-models.sh, or set AMBIENT_MODELS.",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

fn write_wav_16k(path: &Path, samples: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: resample::TARGET_HZ,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    w.finalize()?;
    Ok(())
}

/// Capture both tracks until interrupted, then transcribe and write a session.
/// Name of the file whose existence asks a running `record` to stop. A file
/// rather than a signal because the only launch that gets system audio is
/// LaunchServices', which has no terminal to Ctrl-C and no pid a user can see.
pub const STOP_FILE: &str = "STOP";
/// Live level meter, rewritten each second. Under `open -a` stderr goes
/// nowhere, so this is the only way to tell a live recording from a dead one.
pub const STATUS_FILE: &str = "status";

pub fn record(
    name: Option<&str>,
    bundles: &[String],
    model_dir: Option<&str>,
    seconds: Option<u64>,
) -> Result<PathBuf> {
    let models = models_root()?;
    let asr_dir = match model_dir {
        Some(d) => PathBuf::from(d),
        None => models.join("sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"),
    };
    let vad_path = models.join("silero_vad.onnx");
    if !asr_dir.is_dir() {
        bail!(
            "no ASR model at {} — run ./fetch-models.sh",
            asr_dir.display()
        );
    }
    if !vad_path.is_file() {
        bail!("no VAD model at {}", vad_path.display());
    }

    // A `--app` on the command line beats the stored setting; with no flag the
    // settings window decides.
    let cfg = crate::config::Config::load();
    let bundles: &[String] = if bundles.is_empty() {
        &cfg.apps
    } else {
        bundles
    };
    if !bundles.is_empty() {
        eprintln!("  capturing only: {}", bundles.join(", "));
    }

    let started = chrono::Local::now();
    let slug = name.map(slugify).filter(|s| !s.is_empty());
    let id = match &slug {
        Some(s) => format!("{}-{}", started.format("%Y-%m-%dT%H%M"), s),
        None => started.format("%Y-%m-%dT%H%M").to_string(),
    };
    let dir = home().join(&id);
    let audio = dir.join("audio");
    std::fs::create_dir_all(&audio).with_context(|| format!("creating {}", audio.display()))?;

    let tap = ProcessTap::start(bundles, 60, true, cfg.input_device.as_deref())?;
    // Two devices, two clocks: the mic is on the input device and the call on
    // the tap's aggregate, so they need not share a sample rate.
    let mic_hz = tap.mic_rate as u32;
    let call_hz = tap.call_rate as u32;
    let (mic_ch, call_ch) = (tap.mic_channels, tap.call_channels);
    if call_ch == 0 && mic_ch == 0 {
        bail!("capture started with no channels");
    }

    eprintln!(
        "recording to {}\n  room {} Hz × {} ch, call {} Hz × {} ch — Ctrl-C, or `ambient stop`",
        dir.display(),
        mic_hz,
        mic_ch,
        call_hz,
        call_ch
    );

    // Native-rate scratch. Resampling happens after the recording is safely on
    // disk, so a bug in it costs a rerun and not the conversation.
    let room_native = audio.join("room.native.wav");
    let call_native = audio.join("call.native.wav");
    let spec = |hz: u32| hound::WavSpec {
        channels: 1,
        sample_rate: hz.max(1),
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut room_w = hound::WavWriter::create(&room_native, spec(mic_hz))?;
    let mut call_w = hound::WavWriter::create(&call_native, spec(call_hz))?;

    STOP.store(false, Ordering::SeqCst);
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }

    let stop_file = dir.join(STOP_FILE);
    let status_file = dir.join(STATUS_FILE);
    // A stale sentinel from a previous run would stop this one instantly.
    std::fs::remove_file(&stop_file).ok();
    let deadline = seconds.map(|s| std::time::Instant::now() + std::time::Duration::from_secs(s));

    let t0 = std::time::Instant::now();
    let mut drain = crate::capture::Drain::default();
    let mut frames = 0u64;
    let mut room_peak = 0.0f32;
    let mut call_peak = 0.0f32;
    let mut last_print = std::time::Instant::now();
    let mut max_rendering = 0usize;

    while !STOP.load(Ordering::SeqCst) && !stop_file.exists() {
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        // Already mono, and already padded for any stretch the tap slept
        // through, so the two tracks stay level with each other.
        let (room, call) = tap.drain(&mut drain);

        // The status line has to be written even when nothing is arriving —
        // a silent tap is exactly the case somebody is staring at the menu
        // waiting for a number to move.
        if last_print.elapsed().as_millis() >= 1000 {
            max_rendering = max_rendering.max(crate::probe::processes_rendering_output());
            let secs = t0.elapsed().as_secs();
            let (mic_real, _, call_real, _) = tap.real_seconds(&drain);
            let line = if mic_real + call_real <= 0.0 {
                format!("{:02}:{:02}  no audio arriving", secs / 60, secs % 60)
            } else {
                format!(
                    "{:02}:{:02}  room {room_peak:.3}  call {call_peak:.3}",
                    secs / 60,
                    secs % 60
                )
            };
            eprint!("\r  {line}   ");
            let _ = std::io::stderr().flush();
            std::fs::write(&status_file, format!("recording {line}\n")).ok();
            last_print = std::time::Instant::now();
        }

        if room.is_empty() && call.is_empty() {
            continue;
        }
        frames += room.len().max(call.len()) as u64;

        for s in &room {
            room_peak = room_peak.max(s.abs());
            room_w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
        }
        for s in &call {
            call_peak = call_peak.max(s.abs());
            call_w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
        }
    }

    room_w.finalize()?;
    call_w.finalize()?;
    // Read before the tap goes away: `frames` counts the silence `drain`
    // inserts to keep the tracks level, so it cannot tell a stalled capture
    // from a working one.
    let (mic_real, _, call_real, call_total) = tap.real_seconds(&drain);
    drop(tap);
    std::fs::remove_file(&stop_file).ok();
    std::fs::write(&status_file, "transcribing\n").ok();
    let ended = chrono::Local::now();
    let duration_s = frames as f64 / mic_hz.max(call_hz).max(1) as f64;
    eprintln!("\n  stopped after {duration_s:.1}s — transcribing");
    if call_total - call_real > 1.0 {
        eprintln!(
            "  the call track was idle for {:.1}s of that — padded with silence to stay \
             level with the room",
            call_total - call_real
        );
    }

    if mic_real + call_real <= 0.0 {
        std::fs::write(&status_file, "failed: no audio was captured\n").ok();
        bail!(
            "capture was created (room {mic_hz} Hz, call {call_hz} Hz) but delivered no audio \
             in {:.0}s, \
             so {} holds nothing. This is the silent-capture failure: the recording was never \
             running, whatever the level meter implied.",
            t0.elapsed().as_secs_f64(),
            dir.display()
        );
    }

    if room_peak < 1e-4 {
        eprintln!("  WARNING: the room track is silent — check the microphone grant.");
    }
    if let Some(advice) = crate::capture::silent_tap_advice(call_peak, max_rendering, bundles) {
        eprintln!("\n  WARNING: {advice}\n");
    }

    // One track at a time, so the native buffer is gone before the next is read
    // and neither is alive when the models load.
    for (native, out) in [
        (&room_native, audio.join("room.wav")),
        (&call_native, audio.join("call.wav")),
    ] {
        let (samples, rate) = resample::read_wav_any(native)?;
        let sixteen = resample::to_16k(&samples, rate)?;
        drop(samples);
        write_wav_16k(&out, &sixteen)?;
        std::fs::remove_file(native).ok();
    }

    let mut vad = crate::vad::Vad::load(vad_path.to_str().unwrap())?;
    let mut rec = crate::asr::Recognizer::load(
        asr_dir
            .to_str()
            .ok_or_else(|| anyhow!("model path is not valid UTF-8"))?,
    )?;

    let raw_path = dir.join("raw.jsonl");
    let mut raw = std::fs::File::create(&raw_path)?;
    let mut lines = 0usize;

    for (track, wav) in [
        (Track::Room, audio.join("room.wav")),
        (Track::Call, audio.join("call.wav")),
    ] {
        let (samples, _) = resample::read_wav_any(&wav)?;
        if samples.is_empty() {
            continue;
        }
        // Turns, not merged chunks: a record that spans two people's speech
        // cannot carry a speaker, and `diarize` can only label whole records.
        let chunks = vad.turns(&samples, 30)?;
        let segs = rec.transcribe_segments(&samples, &chunks)?;
        for (seg, text, confidence) in segs {
            let r = RawRecord {
                track,
                start_ms: (seg.start as u64 * 1000) / resample::TARGET_HZ as u64,
                end_ms: (seg.end as u64 * 1000) / resample::TARGET_HZ as u64,
                text,
                confidence,
            };
            writeln!(raw, "{}", serde_json::to_string(&r)?)?;
            lines += 1;
        }
        eprintln!("  {} — {} segment(s)", track.as_str(), chunks.len());
    }
    raw.flush()?;

    // Created empty so the append-only layer always exists to append to.
    let edits_path = dir.join("edits.jsonl");
    if !edits_path.exists() {
        std::fs::File::create(&edits_path)?;
    }

    let meta = SessionMeta {
        id: id.clone(),
        name: name.map(str::to_string),
        started_at: started.to_rfc3339(),
        ended_at: ended.to_rfc3339(),
        duration_s,
        device_hz: call_hz,
        mic_hz,
        channels: call_ch,
        mic_channels: mic_ch,
        apps: bundles.to_vec(),
        model: asr_dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    std::fs::write(
        dir.join("session.json"),
        serde_json::to_string_pretty(&meta)?,
    )?;

    if cfg.diarize && lines > 0 {
        std::fs::write(dir.join(STATUS_FILE), "separating voices\n").ok();
        // Diarization is a nicety; a transcript without speakers still beats
        // losing the recording to a model that failed to load.
        match diarize_session(&dir, cfg.threshold) {
            Ok(n) => eprintln!("  {n} speaker label(s)"),
            Err(e) => eprintln!(
                "  WARNING: could not separate voices ({e}) — \
                                 the transcript is complete but unlabelled"
            ),
        }
    }

    let md = dir.join("transcript.md");
    std::fs::write(&md, markdown(&dir)?)?;
    std::fs::write(dir.join(STATUS_FILE), "done\n").ok();

    // Only now that the text exists is the audio safe to age out. This covers
    // the session just recorded as well as every older one, which is why there
    // is no launchd agent: the app runs whenever a recording happens.
    match sweep_audio(&home(), cfg.audio_retention_days) {
        0 => {}
        n => eprintln!("  {n} track(s) of aged-out audio removed"),
    }

    eprintln!("  {lines} line(s) written\n  {}", md.display());
    Ok(dir)
}

/// The most recently written session, by directory name. Session ids are
/// timestamps, so sorting by name sorts by time without stat-ing anything.
pub fn latest(root: &Path) -> Option<PathBuf> {
    let mut all: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    all.sort();
    all.pop()
}

/// Delete track audio older than the retention window, leaving every transcript
/// untouched. `None` keeps audio forever; `Some(0)` deletes it as soon as the
/// transcript exists.
///
/// Age is taken from the audio file itself rather than its session directory,
/// so renaming a speaker months later does not resurrect the recording.
/// Whether a session directory holds at least one transcribed line. Also the
/// test for "is this a session at all" — anything else in the folder has no
/// `raw.jsonl` and is left alone.
fn has_transcribed_words(session: &Path) -> bool {
    std::fs::read_to_string(session.join("raw.jsonl"))
        .is_ok_and(|t| t.lines().any(|l| !l.trim().is_empty()))
}

pub fn sweep_audio(root: &Path, keep_days: Option<u32>) -> usize {
    sweep_audio_at(root, keep_days, SystemTime::now())
}

pub fn sweep_audio_at(root: &Path, keep_days: Option<u32>, now: SystemTime) -> usize {
    let Some(days) = keep_days else {
        return 0;
    };
    let window = Duration::from_secs(u64::from(days) * 24 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.filter_map(|e| e.ok()) {
        // Never follow a symlink out of the sessions folder: this deletes
        // files, and a link placed here must not make it delete somebody
        // else's.
        if entry
            .path()
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            continue;
        }
        let session = entry.path();
        let audio = session.join("audio");
        // Both halves are needed, and each guards a different way of losing
        // the only copy of a conversation:
        //
        // - no `transcript.md` means the recording is still being written, and
        //   `raw.jsonl` is already growing by then;
        // - a `transcript.md` with no words behind it means ASR found nothing,
        //   which is exactly when the audio is still the only record. `markdown`
        //   writes a transcript saying so, so the file alone proves nothing.
        if !session.join("transcript.md").is_file() || !has_transcribed_words(&session) {
            continue;
        }
        let Ok(tracks) = std::fs::read_dir(&audio) else {
            continue;
        };
        for track in tracks.filter_map(|e| e.ok()).map(|e| e.path()) {
            let old = track
                .metadata()
                .and_then(|m| m.modified())
                .map(|t| now.duration_since(t).unwrap_or_default() >= window)
                .unwrap_or(false);
            if old && std::fs::remove_file(&track).is_ok() {
                removed += 1;
            }
        }
        // Tidy the empty directory, but only if it is genuinely empty — a file
        // this code did not put there is not its business to delete.
        if std::fs::read_dir(&audio).is_ok_and(|mut d| d.next().is_none()) {
            std::fs::remove_dir(&audio).ok();
        }
    }
    removed
}

/// A raw record with whatever the edit layer has said about it.
#[derive(Debug, Clone)]
pub struct Line {
    pub track: Track,
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: Option<String>,
    pub text: String,
}

/// Read `raw.jsonl` and fold `edits.jsonl` over it. With `verbatim`, the edit
/// layer is ignored entirely — the same bytes the recogniser produced.
pub fn transcript(dir: &Path, verbatim: bool) -> Result<Vec<Line>> {
    let raw_text = std::fs::read_to_string(dir.join("raw.jsonl"))
        .with_context(|| format!("reading {}", dir.join("raw.jsonl").display()))?;

    let mut lines: Vec<Line> = Vec::new();
    for l in raw_text.lines().filter(|l| !l.trim().is_empty()) {
        let r: RawRecord = serde_json::from_str(l).context("parsing raw.jsonl")?;
        lines.push(Line {
            track: r.track,
            start_ms: r.start_ms,
            end_ms: r.end_ms,
            speaker: None,
            text: r.text,
        });
    }

    if !verbatim {
        let edits_path = dir.join("edits.jsonl");
        if edits_path.exists() {
            let edits_text = std::fs::read_to_string(&edits_path)?;
            let edits: Vec<Edit> = edits_text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).context("parsing edits.jsonl"))
                .collect::<Result<_>>()?;

            // A revert names an earlier line, so gather them before applying.
            let reverted: std::collections::HashSet<usize> = edits
                .iter()
                .filter_map(|e| match e {
                    Edit::Revert { target_seq, .. } => Some(*target_seq),
                    _ => None,
                })
                .collect();

            for (seq, edit) in edits.iter().enumerate() {
                if reverted.contains(&seq) {
                    continue;
                }
                match edit {
                    Edit::Repair {
                        target, from, to, ..
                    } => {
                        if let Some(l) = lines
                            .iter_mut()
                            .find(|l| l.track == target.track && l.start_ms == target.start_ms)
                        {
                            l.text = l.text.replace(from.as_str(), to.as_str());
                        }
                    }
                    Edit::Speaker { target, name, .. } => {
                        if let Some(l) = lines
                            .iter_mut()
                            .find(|l| l.track == target.track && l.start_ms == target.start_ms)
                        {
                            l.speaker = Some(name.clone());
                        }
                    }
                    Edit::Revert { .. } => {}
                }
            }
        }
    }

    lines.sort_by_key(|l| l.start_ms);
    Ok(lines)
}

pub fn show(dir: &Path, verbatim: bool) -> Result<()> {
    let lines = transcript(dir, verbatim)?;
    if lines.is_empty() {
        eprintln!("no transcript in {}", dir.display());
        return Ok(());
    }
    for l in &lines {
        let secs = l.start_ms / 1000;
        let who = l.speaker.clone().unwrap_or_default();
        println!(
            "[{:02}:{:02}] {:<4} {:<10} {}",
            secs / 60,
            secs % 60,
            l.track.as_str(),
            who,
            l.text
        );
    }
    Ok(())
}

/// Who an automatic speaker edit is attributed to. Load-bearing: re-running
/// `diarize` finds its own previous edits by this name and reverts them, and
/// must not touch a name a person typed.
pub const DIARIZE_BY: &str = "diarize";

/// Assign speakers to an already-recorded session.
///
/// Deliberately not part of `record`. It roughly doubles processing time for
/// something not always wanted, the threshold wants tuning against real room
/// audio, and re-running it is exactly what an append-only format is for — a
/// bad threshold costs an appended revert, not a lost recording.
///
/// Labels are track-prefixed (`room-1`, `call-2`) because speaker 1 in the
/// room and speaker 1 on the call are different people, and nothing downstream
/// should be able to assume otherwise.
pub fn diarize_session(dir: &Path, threshold: f32) -> Result<usize> {
    let raw_text = std::fs::read_to_string(dir.join("raw.jsonl"))
        .with_context(|| format!("reading {}", dir.join("raw.jsonl").display()))?;
    let records: Vec<RawRecord> = raw_text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).context("parsing raw.jsonl"))
        .collect::<Result<_>>()?;
    if records.is_empty() {
        bail!("{} has no transcript to attribute", dir.display());
    }

    // Checked before anything is reverted below. Once retention has swept the
    // audio, a re-run would undo the labels it wrote last time and then find
    // nothing to replace them with — silently unlabelling every speaker that
    // nobody had named by hand.
    let audio = dir.join("audio");
    if !audio.join("room.wav").exists() && !audio.join("call.wav").exists() {
        bail!(
            "{} has no audio left to attribute — the retention setting removed it, \
             and the labels it already carries are all there will be",
            dir.display()
        );
    }

    let root = models_root()?;
    let seg = root.join("pyannote-segmentation-3.0").join("model.onnx");
    let emb = root.join("wespeaker_en_voxceleb_resnet34_LM.onnx");
    for p in [&seg, &emb] {
        if !p.exists() {
            bail!("missing {} — run ./fetch-models.sh", p.display());
        }
    }
    let mut diar = crate::diarize::Diarizer::load(
        seg.to_str().ok_or_else(|| anyhow!("bad model path"))?,
        emb.to_str().ok_or_else(|| anyhow!("bad model path"))?,
    )?;

    let now = chrono::Local::now().to_rfc3339();
    let mut appended: Vec<Edit> = Vec::new();
    let mut protected: std::collections::HashSet<(&str, u64)> = std::collections::HashSet::new();

    // Undo the previous run before writing this one, so history survives and
    // `transcript` does not have to guess which of two labels is current.
    let edits_path = dir.join("edits.jsonl");
    if edits_path.exists() {
        let text = std::fs::read_to_string(&edits_path)?;
        let existing: Vec<Edit> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).context("parsing edits.jsonl"))
            .collect::<Result<_>>()?;
        let already: std::collections::HashSet<usize> = existing
            .iter()
            .filter_map(|e| match e {
                Edit::Revert { target_seq, .. } => Some(*target_seq),
                _ => None,
            })
            .collect();
        for (seq, e) in existing.iter().enumerate() {
            if already.contains(&seq) {
                continue;
            }
            if let Edit::Speaker { by, target, .. } = e {
                if by == DIARIZE_BY {
                    appended.push(Edit::Revert {
                        target_seq: seq,
                        by: DIARIZE_BY.into(),
                        at: now.clone(),
                    });
                } else {
                    // Somebody typed this name. Re-running diarization must not
                    // quietly turn Priya back into call-1.
                    protected.insert((target.track.as_str(), target.start_ms));
                }
            }
        }
    }

    for (track, wav) in [
        (Track::Room, audio.join("room.wav")),
        (Track::Call, audio.join("call.wav")),
    ] {
        if !wav.exists() {
            continue;
        }
        let (samples, rate) = resample::read_wav_any(&wav)?;
        let samples = resample::to_16k(&samples, rate)?;
        if samples.is_empty() {
            continue;
        }
        let spans = diar.diarize(&samples, threshold)?;
        let speakers = spans
            .iter()
            .map(|s| s.speaker)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0);
        eprintln!("  {} — {speakers} speaker(s)", track.as_str());
        if spans.is_empty() {
            continue;
        }

        for r in records.iter().filter(|r| r.track == track) {
            if protected.contains(&(track.as_str(), r.start_ms)) {
                continue;
            }
            let a = r.start_ms * resample::TARGET_HZ as u64 / 1000;
            let b = r.end_ms * resample::TARGET_HZ as u64 / 1000;
            // Whoever holds the most of the segment owns the line. A segment
            // is one VAD-bounded utterance, so a clear majority is the norm
            // and a tie means the cut landed mid-turn.
            let mut overlap: std::collections::HashMap<usize, u64> =
                std::collections::HashMap::new();
            for s in &spans {
                let lo = a.max(s.start as u64);
                let hi = b.min(s.end as u64);
                if hi > lo {
                    *overlap.entry(s.speaker).or_default() += hi - lo;
                }
            }
            if let Some((&spk, _)) = overlap.iter().max_by_key(|(_, &v)| v) {
                appended.push(Edit::Speaker {
                    target: Target {
                        track,
                        start_ms: r.start_ms,
                    },
                    name: format!("{}-{}", track.as_str(), spk + 1),
                    by: DIARIZE_BY.into(),
                    at: now.clone(),
                });
            }
        }
    }

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&edits_path)?;
    for e in &appended {
        writeln!(f, "{}", serde_json::to_string(e)?)?;
    }
    f.flush()?;
    drop(f);
    std::fs::write(dir.join("transcript.md"), markdown(dir)?)?;
    Ok(appended.len())
}

/// Attribution for a name a person typed, as opposed to a cluster id.
pub const USER_BY: &str = "user";

/// Give every line currently labelled `label` a human name. Appends one
/// `Speaker` edit per line, so it is undoable like anything else.
/// Speaker labels nobody has named yet, each with the first thing that voice
/// said. The sample line is what makes the roster picker usable: `call-2` means
/// nothing, but `call-2` next to "right, shall we start with the export spec"
/// is recognisable.
///
/// A label counts as unnamed while it still has diarization's shape,
/// `<track>-<n>`. Once a person types a name it stops matching and drops out.
pub fn unnamed_labels(dir: &Path) -> Result<Vec<(String, String)>> {
    let lines = transcript(dir, false)?;
    let mut out: Vec<(String, String)> = Vec::new();
    for l in &lines {
        let Some(label) = l.speaker.as_deref() else {
            continue;
        };
        let looks_generated = label.rsplit_once('-').is_some_and(|(track, n)| {
            matches!(track, "room" | "call")
                && !n.is_empty()
                && n.chars().all(|c| c.is_ascii_digit())
        });
        if !looks_generated || out.iter().any(|(seen, _)| seen == label) {
            continue;
        }
        out.push((label.to_string(), l.text.trim().to_string()));
    }
    Ok(out)
}

pub fn name_speaker(dir: &Path, label: &str, name: &str) -> Result<usize> {
    let lines = transcript(dir, false)?;
    let targets: Vec<&Line> = lines
        .iter()
        .filter(|l| l.speaker.as_deref() == Some(label))
        .collect();
    if targets.is_empty() {
        let mut seen: Vec<&str> = lines.iter().filter_map(|l| l.speaker.as_deref()).collect();
        seen.sort_unstable();
        seen.dedup();
        let known = if seen.is_empty() {
            "none — run `ambient diarize` first".to_string()
        } else {
            seen.join(", ")
        };
        bail!("no lines labelled {label:?} (available: {known})");
    }

    let now = chrono::Local::now().to_rfc3339();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("edits.jsonl"))?;
    for l in &targets {
        let e = Edit::Speaker {
            target: Target {
                track: l.track,
                start_ms: l.start_ms,
            },
            name: name.to_string(),
            by: USER_BY.into(),
            at: now.clone(),
        };
        writeln!(f, "{}", serde_json::to_string(&e)?)?;
    }
    f.flush()?;
    drop(f);
    std::fs::write(dir.join("transcript.md"), markdown(dir)?)?;
    Ok(targets.len())
}

/// The session currently being captured, if any.
///
/// The native-rate scratch files exist only between the start of a capture and
/// the resample that follows it — which is exactly the window in which a
/// recording can be said to be in progress.
pub fn live_session() -> Option<PathBuf> {
    let mut live: Vec<PathBuf> = std::fs::read_dir(home())
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| is_growing(&p.join("audio").join("room.native.wav")))
        .collect();
    live.sort();
    live.pop()
}

/// Written to within the last few seconds. A process killed mid-recording
/// leaves its scratch wavs behind for ever, and without this check that corpse
/// answers to `ambient stop` and to the menu's level meter — pointing both at a
/// directory nothing is writing to.
fn is_growing(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    meta.modified()
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age.as_secs() < 10)
}

/// Ask a running `record` to stop, by session directory or by finding the one
/// currently in flight. Returns the directory it signalled.
pub fn stop_recording(dir: Option<&Path>) -> Result<PathBuf> {
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        None => live_session()
            .ok_or_else(|| anyhow!("no recording in progress under {}", home().display()))?,
    };
    if !dir.is_dir() {
        bail!("{} is not a session directory", dir.display());
    }
    std::fs::write(dir.join(STOP_FILE), "")?;
    if let Ok(status) = std::fs::read_to_string(dir.join(STATUS_FILE)) {
        eprint!("  {status}");
    }
    Ok(dir)
}

/// How much of the shorter line's wording appears in the longer one, 0.0–1.0.
fn containment(a: &str, b: &str) -> f32 {
    let tok = |s: &str| -> Vec<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect()
    };
    let (x, y) = (tok(a), tok(b));
    let (short, long) = if x.len() <= y.len() {
        (&x, &y)
    } else {
        (&y, &x)
    };
    if short.is_empty() {
        return 0.0;
    }
    let set: std::collections::HashSet<&String> = long.iter().collect();
    short.iter().filter(|t| set.contains(t)).count() as f32 / short.len() as f32
}

/// At or above this, two overlapping lines are the same utterance heard twice.
/// Low enough to survive two independent ASR passes disagreeing on a word or
/// two, high enough that two people saying different things over each other
/// both survive.
const BLEED_SIMILARITY: f32 = 0.6;

/// Drop room lines that are the microphone overhearing the call.
///
/// The room mic picks up the laptop speakers, so on speakerphone every remote
/// utterance is transcribed twice. Only room lines are ever dropped: the call
/// track is both the better copy — 0.87 peak against the mic's 0.26 — and the
/// side that cannot be reconstructed if it goes missing.
pub fn dedup_bleed(lines: &[Line]) -> (Vec<Line>, usize) {
    let calls: Vec<&Line> = lines.iter().filter(|l| l.track == Track::Call).collect();
    let mut kept = Vec::with_capacity(lines.len());
    let mut dropped = 0usize;

    for l in lines {
        if l.track == Track::Call {
            kept.push(l.clone());
            continue;
        }
        let duplicate = calls.iter().any(|c| {
            let lo = l.start_ms.max(c.start_ms);
            let hi = l.end_ms.min(c.end_ms);
            let overlap = hi.saturating_sub(lo);
            let shorter = (l.end_ms - l.start_ms).min(c.end_ms - c.start_ms);
            // Half the shorter span, so a brief coincidence is not enough.
            shorter > 0
                && overlap * 2 >= shorter
                && containment(&l.text, &c.text) >= BLEED_SIMILARITY
        });
        if duplicate {
            dropped += 1;
        } else {
            kept.push(l.clone());
        }
    }
    (kept, dropped)
}

fn mmss(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// The session as a markdown document, for whatever files it onward.
///
/// Derived, never authoritative: `raw.jsonl` and `edits.jsonl` remain the
/// source of truth, so regenerating this file loses nothing.
pub fn markdown(dir: &Path) -> Result<String> {
    let lines = transcript(dir, false)?;
    let (lines, suppressed) = dedup_bleed(&lines);

    let meta: Option<SessionMeta> = std::fs::read_to_string(dir.join("session.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());

    let mut speakers: Vec<&str> = Vec::new();
    for l in lines.iter().filter_map(|l| l.speaker.as_deref()) {
        if !speakers.contains(&l) {
            speakers.push(l);
        }
    }

    // JSON string escaping is valid YAML double-quoted scalar escaping, so this
    // survives a session named with a colon or a quote.
    let q = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
    let mut out = String::from("---\n");
    if let Some(m) = &meta {
        out.push_str(&format!("id: {}\n", q(&m.id)));
        if let Some(n) = &m.name {
            out.push_str(&format!("name: {}\n", q(n)));
        }
        out.push_str(&format!("started_at: {}\n", q(&m.started_at)));
        out.push_str(&format!("ended_at: {}\n", q(&m.ended_at)));
        out.push_str(&format!("duration_s: {:.1}\n", m.duration_s));
        if !m.apps.is_empty() {
            let apps: Vec<String> = m.apps.iter().map(|a| q(a)).collect();
            out.push_str(&format!("apps: [{}]\n", apps.join(", ")));
        }
        out.push_str(&format!("model: {}\n", q(&m.model)));
    }
    let sp: Vec<String> = speakers.iter().map(|s| q(s)).collect();
    out.push_str(&format!("speakers: [{}]\n", sp.join(", ")));
    out.push_str("---\n\n");

    let title = meta
        .as_ref()
        .and_then(|m| m.name.clone())
        .or_else(|| meta.as_ref().map(|m| m.id.clone()))
        .unwrap_or_else(|| "Transcript".into());
    out.push_str(&format!("# {title}\n\n"));

    if lines.is_empty() {
        out.push_str("_No speech was transcribed._\n");
        return Ok(out);
    }

    for l in &lines {
        match &l.speaker {
            Some(name) => out.push_str(&format!(
                "**[{}] {}** ({})\n",
                mmss(l.start_ms),
                name,
                l.track.as_str()
            )),
            None => out.push_str(&format!(
                "**[{}] {}**\n",
                mmss(l.start_ms),
                l.track.as_str()
            )),
        }
        out.push_str(&format!("{}\n\n", l.text.trim()));
    }

    if suppressed > 0 {
        out.push_str(&format!(
            "---\n_{suppressed} room line(s) suppressed as call-track duplicates._\n"
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session directory as `record()` leaves one: a transcript, some raw
    /// lines, and the two track wavs. Built in a temp dir — the sweep deletes
    /// files, so it must never be pointed at the real sessions folder.
    fn fake_session(root: &Path, id: &str, with_transcript: bool) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        std::fs::write(dir.join("audio").join("room.wav"), b"RIFF....").unwrap();
        std::fs::write(dir.join("audio").join("call.wav"), b"RIFF....").unwrap();
        let line = RawRecord {
            track: Track::Room,
            start_ms: 0,
            end_ms: 1000,
            text: "some words were said".into(),
            confidence: 0.9,
        };
        std::fs::write(
            dir.join("raw.jsonl"),
            format!("{}\n", serde_json::to_string(&line).unwrap()),
        )
        .unwrap();
        if with_transcript {
            std::fs::write(dir.join("transcript.md"), "# transcript\n").unwrap();
        }
        dir
    }

    fn sweep_root(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ambient-sweep-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&p).ok();
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn keeping_audio_forever_deletes_nothing() {
        let root = sweep_root("forever");
        let dir = fake_session(&root, "2026-01-01T0900", true);
        assert_eq!(sweep_audio(&root, None), 0);
        assert!(dir.join("audio").join("room.wav").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn zero_days_removes_the_audio_and_keeps_the_words() {
        let root = sweep_root("zero");
        let dir = fake_session(&root, "2026-01-01T0900", true);
        assert_eq!(sweep_audio(&root, Some(0)), 2);
        assert!(!dir.join("audio").exists());
        assert!(dir.join("transcript.md").exists());
        assert!(dir.join("raw.jsonl").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn audio_within_the_window_is_left_alone() {
        let root = sweep_root("window");
        let dir = fake_session(&root, "2026-01-01T0900", true);
        // Seven days of retention, judged one day after the file was written.
        let a_day_later = SystemTime::now() + Duration::from_secs(24 * 60 * 60);
        assert_eq!(sweep_audio_at(&root, Some(7), a_day_later), 0);
        assert!(dir.join("audio").join("room.wav").exists());

        // ...and eight days after, it goes.
        let later = SystemTime::now() + Duration::from_secs(8 * 24 * 60 * 60);
        assert_eq!(sweep_audio_at(&root, Some(7), later), 2);
        assert!(!dir.join("audio").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    /// The one that would hurt most. `markdown` writes a transcript saying
    /// "no speech was transcribed" when ASR finds nothing, so a file-exists
    /// check would delete the audio of a recording that produced no text —
    /// destroying the only copy precisely when transcription failed.
    #[test]
    fn a_transcript_with_no_words_in_it_does_not_authorise_deleting_the_audio() {
        let root = sweep_root("nowords");
        let dir = fake_session(&root, "2026-01-01T0900", true);
        std::fs::write(dir.join("raw.jsonl"), "").unwrap();
        assert_eq!(sweep_audio(&root, Some(0)), 0);
        assert!(dir.join("audio").join("room.wav").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    /// The sweep deletes files, so anything in the folder that is not a session
    /// is none of its business.
    #[test]
    fn a_directory_that_is_not_a_session_is_left_alone() {
        let root = sweep_root("notasession");
        let other = root.join("my-notes");
        std::fs::create_dir_all(other.join("audio")).unwrap();
        std::fs::write(other.join("audio").join("room.wav"), b"mine").unwrap();
        std::fs::write(other.join("transcript.md"), "not ambient's").unwrap();
        assert_eq!(sweep_audio(&root, Some(0)), 0);
        assert!(other.join("audio").join("room.wav").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    /// The one that would hurt: a recording still being transcribed has no
    /// transcript yet, and its audio is the only copy of what was said.
    #[test]
    fn a_session_without_a_transcript_is_never_swept() {
        let root = sweep_root("inflight");
        let dir = fake_session(&root, "2026-01-01T0900", false);
        assert_eq!(sweep_audio(&root, Some(0)), 0);
        assert!(dir.join("audio").join("room.wav").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_latest_session_is_the_last_by_name() {
        let root = sweep_root("latest");
        fake_session(&root, "2026-01-01T0900", true);
        fake_session(&root, "2026-03-04T1130", true);
        fake_session(&root, "2026-02-01T0900", true);
        assert_eq!(
            latest(&root).unwrap().file_name().unwrap(),
            "2026-03-04T1130"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A session with two raw lines and whatever edits are handed in, written
    /// the way `record` and `diarize_session` write them.
    fn session_with(root: &Path, edits: &[Edit]) -> PathBuf {
        let dir = root.join("2026-01-01T0900");
        std::fs::create_dir_all(&dir).unwrap();
        let raw = [
            RawRecord {
                track: Track::Call,
                start_ms: 0,
                end_ms: 2000,
                text: "shall we start with the export spec".into(),
                confidence: 0.9,
            },
            RawRecord {
                track: Track::Room,
                start_ms: 2100,
                end_ms: 4000,
                text: "yes, go ahead".into(),
                confidence: 0.9,
            },
        ];
        let body: String = raw
            .iter()
            .map(|r| format!("{}\n", serde_json::to_string(r).unwrap()))
            .collect();
        std::fs::write(dir.join("raw.jsonl"), body).unwrap();
        let body: String = edits
            .iter()
            .map(|e| format!("{}\n", serde_json::to_string(e).unwrap()))
            .collect();
        std::fs::write(dir.join("edits.jsonl"), body).unwrap();
        dir
    }

    fn spoken_by(track: Track, start_ms: u64, name: &str, by: &str) -> Edit {
        Edit::Speaker {
            target: Target { track, start_ms },
            name: name.into(),
            by: by.into(),
            at: "2026-01-01T09:00:00+00:00".into(),
        }
    }

    #[test]
    fn diarizations_own_labels_are_the_ones_offered_for_naming() {
        let root = sweep_root("unnamed");
        let dir = session_with(
            &root,
            &[
                spoken_by(Track::Call, 0, "call-1", DIARIZE_BY),
                spoken_by(Track::Room, 2100, "room-1", DIARIZE_BY),
            ],
        );
        let unnamed = unnamed_labels(&dir).unwrap();
        assert_eq!(unnamed.len(), 2);
        assert_eq!(unnamed[0].0, "call-1");
        // The sample line is the point: `call-1` alone is unrecognisable.
        assert_eq!(unnamed[0].1, "shall we start with the export spec");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_person_with_a_name_is_no_longer_offered() {
        let root = sweep_root("named");
        let dir = session_with(
            &root,
            &[
                spoken_by(Track::Call, 0, "call-1", DIARIZE_BY),
                spoken_by(Track::Room, 2100, "room-1", DIARIZE_BY),
                spoken_by(Track::Call, 0, "Priya", USER_BY),
            ],
        );
        let unnamed = unnamed_labels(&dir).unwrap();
        assert_eq!(unnamed.len(), 1);
        assert_eq!(unnamed[0].0, "room-1");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Someone genuinely called "call-1" is not a case worth handling, but a
    /// person named "Room-2" would be — the shape test must not eat a real name.
    #[test]
    fn a_name_that_merely_resembles_a_label_is_left_named() {
        let root = sweep_root("resembles");
        let dir = session_with(
            &root,
            &[
                spoken_by(Track::Call, 0, "call-one", USER_BY),
                spoken_by(Track::Room, 2100, "roomba", USER_BY),
            ],
        );
        assert!(unnamed_labels(&dir).unwrap().is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    /// Retention and diarization interact badly if this is not guarded: the
    /// re-run reverts last time's labels before it discovers there is nothing
    /// left to read, which would unlabel everyone nobody had named by hand.
    #[test]
    fn diarizing_a_session_whose_audio_was_swept_refuses_rather_than_unlabelling() {
        let root = sweep_root("swept");
        let dir = session_with(&root, &[spoken_by(Track::Call, 0, "call-1", DIARIZE_BY)]);
        std::fs::write(dir.join("transcript.md"), "# t\n").unwrap();
        let before = std::fs::read_to_string(dir.join("edits.jsonl")).unwrap();

        let e = diarize_session(&dir, 0.5).unwrap_err();
        assert!(e.to_string().contains("no audio left"), "{e}");
        // The important half: it must not have appended a revert on its way out.
        assert_eq!(
            std::fs::read_to_string(dir.join("edits.jsonl")).unwrap(),
            before
        );
        std::fs::remove_dir_all(&root).ok();
    }

    fn line(track: Track, start_ms: u64, end_ms: u64, text: &str) -> Line {
        Line {
            track,
            start_ms,
            end_ms,
            speaker: None,
            text: text.into(),
        }
    }

    /// Both strings are real: the same utterance as the mic and the tap each
    /// transcribed it in the diarization end-to-end.
    #[test]
    fn the_microphone_overhearing_the_call_is_dropped() {
        let lines = vec![
            line(
                Track::Room,
                90,
                9480,
                "Right, let's start with the Transcoda backlog. It cleared overnight and the retry storm has not come back. So I think we can call that one closed for now.",
            ),
            line(
                Track::Call,
                60,
                9580,
                "Right, let's start with the Transcoda backlog. It cleared overnight and the retry-storm has not come back, so I think we can call that one closed for now.",
            ),
        ];
        let (kept, dropped) = dedup_bleed(&lines);
        assert_eq!(dropped, 1);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].track, Track::Call);
    }

    /// The one that matters: real cross-talk must survive. A dedup that eats
    /// speech is worse than no dedup at all.
    #[test]
    fn genuine_cross_talk_is_kept() {
        let lines = vec![
            line(
                Track::Room,
                1000,
                5000,
                "Sorry, could you repeat the last part?",
            ),
            line(
                Track::Call,
                900,
                5200,
                "and then we shipped the chunking fix on Tuesday afternoon",
            ),
        ];
        let (kept, dropped) = dedup_bleed(&lines);
        assert_eq!(dropped, 0);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn the_same_words_at_a_different_time_are_kept() {
        let lines = vec![
            line(Track::Room, 60_000, 64_000, "let's start with the backlog"),
            line(Track::Call, 0, 4_000, "let's start with the backlog"),
        ];
        let (_, dropped) = dedup_bleed(&lines);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn a_room_only_session_keeps_everything() {
        let lines = vec![
            line(Track::Room, 0, 4_000, "morning all"),
            line(Track::Room, 5_000, 9_000, "shall we start"),
        ];
        let (kept, dropped) = dedup_bleed(&lines);
        assert_eq!(dropped, 0);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn containment_ignores_case_and_punctuation() {
        assert!((containment("Retry-storm, gone!", "retry storm gone") - 1.0).abs() < 1e-6);
        assert!(containment("completely different words here", "retry storm gone") < 0.2);
    }
}
