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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
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
    /// What the capture wanted to say and could only say to stderr: a silent
    /// room track, a tap that returned nothing. Stored so it is still there
    /// when the session is opened tomorrow. `default` keeps sessions written
    /// before this field loadable.
    #[serde(default)]
    pub warnings: Vec<String>,
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

/// Which stage of a recording's life the worker is in, as told to a
/// [`Meter`]. Distinct from `Phase` in `state.rs`: that governs which
/// transitions are legal, this is what the worker itself is doing right now.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterPhase {
    Capturing = 0,
    Transcribing = 1,
    Diarizing = 2,
    Done = 3,
    Failed = 4,
}

impl MeterPhase {
    fn from_u8(v: u8) -> MeterPhase {
        match v {
            1 => MeterPhase::Transcribing,
            2 => MeterPhase::Diarizing,
            3 => MeterPhase::Done,
            4 => MeterPhase::Failed,
            _ => MeterPhase::Capturing,
        }
    }
}

/// A live recording's level meter, shared between the worker thread and
/// whichever UI is drawing it.
///
/// This is deliberately not the same numbers `record_into` already tracks for
/// [`crate::capture::silent_tap_advice`] and the room-silent warning below:
/// those are cumulative over the whole capture, and are the app's only guard
/// against a capture that reports success and contains silence — resetting
/// them would make a dead mic read as healthy the moment the meter cleared.
/// `room_peak_milli`/`call_peak_milli` here answer a different question, "is
/// audio arriving *right now*", and are reset every second for exactly that
/// reason. Peaks are stored as thousandths of full scale so an `f32` amplitude
/// fits an atomic integer.
pub struct Meter {
    pub elapsed_ms: AtomicU64,
    pub room_peak_milli: AtomicU32,
    pub call_peak_milli: AtomicU32,
    pub audio_arriving: AtomicBool,
    pub phase: AtomicU8,
}

impl Default for Meter {
    fn default() -> Self {
        Meter {
            elapsed_ms: AtomicU64::new(0),
            room_peak_milli: AtomicU32::new(0),
            call_peak_milli: AtomicU32::new(0),
            audio_arriving: AtomicBool::new(false),
            phase: AtomicU8::new(MeterPhase::Capturing as u8),
        }
    }
}

impl Meter {
    pub fn phase(&self) -> MeterPhase {
        MeterPhase::from_u8(self.phase.load(Ordering::Relaxed))
    }

    pub(crate) fn set_phase(&self, p: MeterPhase) {
        self.phase.store(p as u8, Ordering::Relaxed);
    }

    /// One line, matching the shape of the status file for the phases that
    /// have a level to show, and reading the per-interval peaks — so a muted
    /// mic falls back to zero within a second rather than holding whatever it
    /// last saw.
    pub fn status_line(&self) -> String {
        let elapsed = self.elapsed_ms.load(Ordering::Relaxed) / 1000;
        let (m, s) = (elapsed / 60, elapsed % 60);
        match self.phase() {
            MeterPhase::Capturing => {
                if self.audio_arriving.load(Ordering::Relaxed) {
                    let room = self.room_peak_milli.load(Ordering::Relaxed) as f32 / 1000.0;
                    let call = self.call_peak_milli.load(Ordering::Relaxed) as f32 / 1000.0;
                    format!("{m:02}:{s:02} · room {room:.2} · call {call:.2}")
                } else {
                    format!("{m:02}:{s:02}  no audio arriving")
                }
            }
            MeterPhase::Transcribing => "transcribing…".to_string(),
            MeterPhase::Diarizing => "separating voices…".to_string(),
            MeterPhase::Done => "done".to_string(),
            MeterPhase::Failed => "failed".to_string(),
        }
    }
}

/// A session directory this process created and therefore owns.
///
/// The only way to hold one is [`SessionDir::claim`], and claiming is what
/// creates the directory — so a `SessionDir` is evidence that no other
/// recording is writing there. That is what stops two recordings started in
/// the same clock minute from sharing the directory their timestamp names.
#[derive(Debug)]
pub struct SessionDir(PathBuf);

impl SessionDir {
    /// The only constructor. `create_dir` — not `create_dir_all` — fails with
    /// `AlreadyExists` rather than succeeding into an occupied directory, so
    /// the claim is atomic against anything else racing for the same name.
    ///
    /// A collision retries with a **zero-padded** suffix (`-02`, `-03`).
    /// `latest()` and every listing sort these names as bytes, and unpadded
    /// `-10` sorts before `-2`.
    pub fn claim(home: &Path, name: Option<&str>) -> Result<Self> {
        std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
        let started = chrono::Local::now();
        let slug = name.map(slugify).filter(|s| !s.is_empty());
        let base = match &slug {
            Some(s) => format!("{}-{}", started.format("%Y-%m-%dT%H%M"), s),
            None => started.format("%Y-%m-%dT%H%M").to_string(),
        };
        for n in 1..=99u32 {
            let id = if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n:02}")
            };
            let dir = home.join(&id);
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(SessionDir(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(e).with_context(|| format!("creating {}", dir.display()));
                }
            }
        }
        bail!(
            "{} already holds 99 sessions claimed as {base}",
            home.display()
        )
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The session id, which is the directory's own name and never the string
    /// that was formatted to ask for it: a claim that collided is called
    /// `-02`, and everything written inside has to say so.
    pub fn id(&self) -> String {
        self.0
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

impl AsRef<Path> for SessionDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

/// Proof that a stop was asked for, and of nothing else. The unit field is
/// private, so [`signal_stop`] is the only thing in the program that can make
/// one — a "stopped" state cannot be constructed without having stopped.
#[derive(Debug)]
pub struct StopSignalled(());

/// Ask the recording in `dir` to stop. Unlike [`stop_recording`] this cannot
/// signal the wrong directory: it is told which one, by a value only a claim
/// could have produced, so moving the sessions folder mid-recording does not
/// make the running capture unreachable.
pub fn signal_stop(dir: &SessionDir) -> Result<StopSignalled> {
    let p = dir.path().join(STOP_FILE);
    std::fs::write(&p, "").with_context(|| format!("writing {}", p.display()))?;
    Ok(StopSignalled(()))
}

/// Removes a just-claimed directory unless the recording got far enough for it
/// to hold something. Disarmed as soon as the tap is running: past that point
/// the directory may hold the only copy of a conversation, and an empty
/// directory left behind is the cheaper mistake by a wide margin.
struct ClaimGuard<'a>(Option<&'a Path>);

impl ClaimGuard<'_> {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for ClaimGuard<'_> {
    fn drop(&mut self) {
        if let Some(dir) = self.0 {
            std::fs::remove_dir_all(dir).ok();
        }
    }
}

/// Held by whichever process is transcribing a session. Two transcribers on
/// one directory would race to truncate `raw.jsonl`, and the launch-time
/// re-queue cannot otherwise tell a transcriber that is running from one
/// that died.
pub const TRANSCRIBING_LOCK: &str = "transcribing.lock";

/// Proof that this process holds the transcription lock for a session, for
/// as long as it lives. Only [`claim_transcription`] makes one.
#[derive(Debug)]
pub struct TranscribeLock(PathBuf);

impl Drop for TranscribeLock {
    fn drop(&mut self) {
        // Only a lock that still names this process. If it was ever judged
        // stale and taken over, the file is somebody else's now.
        let ours = std::fs::read_to_string(&self.0)
            .is_ok_and(|s| s.trim() == std::process::id().to_string());
        if ours {
            std::fs::remove_file(&self.0).ok();
        }
    }
}

/// The pid written into a lock file, if the file names a live process. A pid
/// that no longer answers `kill(pid, 0)` is a crash's leftovers, and the
/// lock is stale.
fn live_transcriber(dir: &Path) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(dir.join(TRANSCRIBING_LOCK))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    // Signal 0 delivers nothing and only reports whether the pid exists.
    // Safe: no memory is involved and a wrong pid is answered, not acted on.
    (unsafe { libc::kill(pid as libc::pid_t, 0) } == 0).then_some(pid)
}

/// Take the transcription lock for `dir`, or say who holds it. A stale lock
/// from a dead process is taken over; a live one is refused.
///
/// The pid is written to a private file first and hard-linked into place:
/// `link` fails if the lock exists and otherwise publishes a file that
/// already holds its contents, so no reader can ever see an empty lock and
/// mistake a live claim for a stale one.
pub fn claim_transcription(dir: &Path) -> Result<TranscribeLock> {
    let path = dir.join(TRANSCRIBING_LOCK);
    let pid = std::process::id();
    let staging = dir.join(format!("{TRANSCRIBING_LOCK}.{pid}"));
    std::fs::write(&staging, pid.to_string())
        .with_context(|| format!("writing {}", staging.display()))?;
    let result = (|| {
        for _ in 0..2 {
            match std::fs::hard_link(&staging, &path) {
                Ok(()) => return Ok(TranscribeLock(path.clone())),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if let Some(holder) = live_transcriber(dir) {
                        bail!(
                            "{} is being transcribed by another process (pid {holder})",
                            dir.display()
                        );
                    }
                    // Dead holder: clear it and try once more. A second
                    // AlreadyExists means someone else won the retry.
                    std::fs::remove_file(&path).ok();
                }
                Err(e) => return Err(e).with_context(|| format!("creating {}", path.display())),
            }
        }
        bail!("{} was claimed by another transcriber", dir.display())
    })();
    std::fs::remove_file(&staging).ok();
    result
}

/// Claim a directory under the sessions folder and record into it.
pub fn record(
    name: Option<&str>,
    bundles: &[String],
    model_dir: Option<&str>,
    seconds: Option<u64>,
) -> Result<PathBuf> {
    let dir = SessionDir::claim(&home(), name)?;
    record_into(&dir, name, bundles, model_dir, seconds, None)
}

/// Record into a directory that has already been claimed, then transcribe
/// it, blocking for both. This creates no session directory of its own, so
/// "record a second session into an occupied one" is not expressible by any
/// caller, present or future.
///
/// The two halves are [`capture_into`] and [`transcribe_session`]. The CLI
/// runs them back to back through this; the menu bar app runs the first on
/// the capture thread and hands the second to its transcription queue, so a
/// new recording can start while the last one is still being transcribed.
pub fn record_into(
    dir: &SessionDir,
    name: Option<&str>,
    bundles: &[String],
    model_dir: Option<&str>,
    seconds: Option<u64>,
    meter: Option<std::sync::Arc<Meter>>,
) -> Result<PathBuf> {
    let path = capture_into(dir, name, bundles, model_dir, seconds, meter.clone())?;
    transcribe_session(&path, model_dir, meter)
}

/// Where the ASR and VAD models are, checked to exist. Called by both halves:
/// the capture half so a missing model fails before the tap starts, and the
/// transcription half because it is the one that loads them — and by the `wer`
/// harness, so what it scores is the model a recording would have used.
pub fn model_paths(model_dir: Option<&str>) -> Result<(PathBuf, PathBuf)> {
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
    Ok((asr_dir, vad_path))
}

/// Capture into a claimed directory until stopped, and leave the audio on
/// disk ready to transcribe: 16 kHz `audio/room.wav` and `audio/call.wav`,
/// `session.json`, and `status` reading `captured`.
///
/// `name` is the human title stored in `session.json`; the id is the claimed
/// directory's own name and comes from `dir`. `meter`, when given, is written
/// alongside the status file for a live UI to read without touching the
/// filesystem.
pub fn capture_into(
    dir: &SessionDir,
    name: Option<&str>,
    bundles: &[String],
    model_dir: Option<&str>,
    seconds: Option<u64>,
    meter: Option<std::sync::Arc<Meter>>,
) -> Result<PathBuf> {
    let mut guard = ClaimGuard(Some(dir.path()));
    let id = dir.id();
    let dir = dir.path();
    let (asr_dir, _) = model_paths(model_dir)?;

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
    // Everything that can fail before this point fails with nothing recorded.
    // Everything after it may be sitting on audio, so the directory stays.
    guard.disarm();

    STOP.store(false, Ordering::SeqCst);
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }

    let stop_file = dir.join(STOP_FILE);
    let status_file = dir.join(STATUS_FILE);
    // No stale-sentinel sweep here: the directory was claimed exclusively a
    // moment ago, so it cannot hold one. Deleting a STOP written *since* the
    // claim would swallow a real stop and leave the capture running under a
    // caller that believes it stopped.
    let deadline = seconds.map(|s| std::time::Instant::now() + std::time::Duration::from_secs(s));

    let t0 = std::time::Instant::now();
    let mut drain = crate::capture::Drain::default();
    let mut frames = 0u64;
    let mut room_peak = 0.0f32;
    let mut call_peak = 0.0f32;
    // Beside the cumulative peaks above, not instead of them: these reset
    // every time the meter is written, so a mic that goes silent mid-capture
    // reads as silent within a second rather than holding its last peak.
    let mut interval_room_peak = 0.0f32;
    let mut interval_call_peak = 0.0f32;
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
            if let Some(m) = &meter {
                m.elapsed_ms
                    .store(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
                m.room_peak_milli
                    .store((interval_room_peak * 1000.0) as u32, Ordering::Relaxed);
                m.call_peak_milli
                    .store((interval_call_peak * 1000.0) as u32, Ordering::Relaxed);
                m.audio_arriving
                    .store(mic_real + call_real > 0.0, Ordering::Relaxed);
                m.set_phase(MeterPhase::Capturing);
            }
            interval_room_peak = 0.0;
            interval_call_peak = 0.0;
            last_print = std::time::Instant::now();
        }

        if room.is_empty() && call.is_empty() {
            continue;
        }
        frames += room.len().max(call.len()) as u64;

        for s in &room {
            room_peak = room_peak.max(s.abs());
            interval_room_peak = interval_room_peak.max(s.abs());
            room_w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
        }
        for s in &call {
            call_peak = call_peak.max(s.abs());
            interval_call_peak = interval_call_peak.max(s.abs());
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
    // `Done` here means the *capture* is done: the tap is gone and the only
    // work left on this thread is writing what it heard. The window draws it
    // as "Finishing", which is the honest word for the seconds of resampling
    // that follow a Stop.
    std::fs::write(&status_file, "finishing\n").ok();
    if let Some(m) = &meter {
        m.set_phase(MeterPhase::Done);
    }
    let ended = chrono::Local::now();
    let duration_s = frames as f64 / mic_hz.max(call_hz).max(1) as f64;
    eprintln!("\n  stopped after {duration_s:.1}s — writing audio");
    if call_total - call_real > 1.0 {
        eprintln!(
            "  the call track was idle for {:.1}s of that — padded with silence to stay \
             level with the room",
            call_total - call_real
        );
    }

    if mic_real + call_real <= 0.0 {
        std::fs::write(&status_file, "failed: no audio was captured\n").ok();
        if let Some(m) = &meter {
            m.set_phase(MeterPhase::Failed);
        }
        bail!(
            "capture was created (room {mic_hz} Hz, call {call_hz} Hz) but delivered no audio \
             in {:.0}s, \
             so {} holds nothing. This is the silent-capture failure: the recording was never \
             running, whatever the level meter implied.",
            t0.elapsed().as_secs_f64(),
            dir.display()
        );
    }

    // Cumulative peaks over the whole capture, untouched: these two checks are
    // the only guard against a capture that reports success and contains
    // silence, and a per-interval peak would answer a different question.
    let mut warnings: Vec<String> = Vec::new();
    if room_peak < 1e-4 {
        eprintln!("  WARNING: the room track is silent — check the microphone grant.");
        warnings.push("the room track is silent — check the microphone grant.".into());
    }
    if let Some(advice) = crate::capture::silent_tap_advice(call_peak, max_rendering, bundles) {
        eprintln!("\n  WARNING: {advice}\n");
        warnings.push(advice);
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

    // Written here, at the end of the capture, rather than after ASR: the
    // session is complete as a *recording* now, and a transcript that never
    // arrives must not leave it looking like a capture that was killed.
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
        warnings,
    };
    std::fs::write(
        dir.join("session.json"),
        serde_json::to_string_pretty(&meta)?,
    )?;
    // `captured` is the one status only this half writes, and the one
    // `captured_awaiting_transcript` looks for after a crash.
    std::fs::write(&status_file, "captured\n").ok();
    eprintln!("  audio written — {:.1}s", duration_s);
    Ok(dir.to_path_buf())
}

/// Transcribe a session whose audio [`capture_into`] has already written:
/// ASR over both tracks into `raw.jsonl`, diarisation if configured, and
/// `transcript.md`. Runs on whichever thread calls it — the CLI's, or the
/// menu bar app's serial transcription queue — and never beside a tap this
/// process owns.
pub fn transcribe_session(
    dir: &Path,
    model_dir: Option<&str>,
    meter: Option<std::sync::Arc<Meter>>,
) -> Result<PathBuf> {
    let r = transcribe_inner(dir, model_dir, meter.as_ref());
    if let Err(e) = &r {
        // The session's own record of what went wrong. The menu bar app may
        // be recording something else when this lands and have nowhere to
        // draw it yet; the file is there whenever the session is opened.
        std::fs::write(dir.join(STATUS_FILE), format!("failed: {e:#}\n")).ok();
        if let Some(m) = &meter {
            m.set_phase(MeterPhase::Failed);
        }
    }
    r
}

fn transcribe_inner(
    dir: &Path,
    model_dir: Option<&str>,
    meter: Option<&std::sync::Arc<Meter>>,
) -> Result<PathBuf> {
    // Taken before anything is written, held until the transcript is. A
    // second transcriber — the CLI and the app on the same session — is
    // refused here rather than racing it for `raw.jsonl`.
    let _lock = claim_transcription(dir)?;
    let (asr_dir, vad_path) = model_paths(model_dir)?;
    let cfg = crate::config::Config::load();
    let audio = dir.join("audio");
    let status_file = dir.join(STATUS_FILE);
    std::fs::write(&status_file, "transcribing\n").ok();
    if let Some(m) = &meter {
        m.set_phase(MeterPhase::Transcribing);
    }

    // Created before the models load, not after: its existence is what
    // separates a session being transcribed from one killed mid-capture in
    // the window's "Interrupted" test, and a model load takes seconds.
    let raw_path = dir.join("raw.jsonl");
    let mut raw = std::fs::File::create(&raw_path)?;
    let mut lines = 0usize;

    let mut vad = crate::vad::Vad::load(vad_path.to_str().unwrap())?;
    let mut rec = crate::asr::Recognizer::load(
        asr_dir
            .to_str()
            .ok_or_else(|| anyhow!("model path is not valid UTF-8"))?,
    )?;

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

    if cfg.diarize && lines > 0 {
        std::fs::write(dir.join(STATUS_FILE), "separating voices\n").ok();
        if let Some(m) = &meter {
            m.set_phase(MeterPhase::Diarizing);
        }
        // Diarization is a nicety; a transcript without speakers still beats
        // losing the recording to a model that failed to load.
        match diarize_session(dir, cfg.threshold) {
            Ok(n) => eprintln!("  {n} speaker label(s)"),
            Err(e) => eprintln!(
                "  WARNING: could not separate voices ({e}) — \
                                 the transcript is complete but unlabelled"
            ),
        }
    }

    let md = dir.join("transcript.md");
    std::fs::write(&md, markdown(dir)?)?;
    std::fs::write(dir.join(STATUS_FILE), "done\n").ok();
    if let Some(m) = &meter {
        m.set_phase(MeterPhase::Done);
    }

    // Only now that the text exists is the audio safe to age out. This covers
    // the session just recorded as well as every older one, which is why there
    // is no launchd agent: the app runs whenever a recording happens.
    match sweep_audio(&home(), cfg.audio_retention_days) {
        0 => {}
        n => eprintln!("  {n} track(s) of aged-out audio removed"),
    }

    eprintln!("  {lines} line(s) written\n  {}", md.display());
    Ok(dir.to_path_buf())
}

/// Every session directory under `root`, oldest first. Session ids are
/// timestamps, so sorting by name sorts by time without stat-ing anything —
/// and the `is_dir` filter is what keeps a loose file in the folder (`app.log`
/// sorts after every `2…` id) out of the answer.
///
/// This is the one enumeration of the sessions folder. Anything else walking
/// it with `read_dir` is the same bug in a new place.
pub fn list(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut all: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    all.sort();
    all
}

/// The most recently written session. Built on [`list`] so a second walk
/// cannot drift from the first.
pub fn latest(root: &Path) -> Option<PathBuf> {
    list(root).pop()
}

/// Sessions whose audio was written and whose transcript never was, oldest
/// first — what a crash mid-queue leaves behind, and what the menu bar app
/// re-queues at launch.
///
/// The shape is `session.json` present and `transcript.md` absent, with
/// audio to transcribe. `raw.jsonl` is deliberately *not* consulted: a crash
/// after the transcriber created it but before it finished would otherwise
/// leave the session excluded for ever, and `transcript.md` is the last thing
/// written — the same test [`sweep_audio`] uses for "finished". Sessions from
/// before the split wrote `session.json` after ASR, so they qualify only if
/// they were killed in that window, which is exactly when they should. A
/// native scratch wav still growing means a capture is in flight in another
/// process, and a [`TRANSCRIBING_LOCK`] naming a live pid means a transcriber
/// is; neither session is ours to touch.
pub fn captured_awaiting_transcript(root: &Path) -> Vec<PathBuf> {
    list(root)
        .into_iter()
        .filter(|dir| {
            let audio = dir.join("audio");
            dir.join("session.json").is_file()
                && !dir.join("transcript.md").is_file()
                && (audio.join("room.wav").is_file() || audio.join("call.wav").is_file())
                && !is_growing(&audio.join("room.native.wav"))
                && live_transcriber(dir).is_none()
        })
        .collect()
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
pub fn is_growing(p: &Path) -> bool {
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

    /// Bug 4. Two recordings starting inside the same clock minute used to be
    /// handed the same directory name, and the second `create_dir_all`
    /// succeeded straight into the first's session — overwriting its
    /// transcript with its own.
    #[test]
    fn two_claims_in_one_clock_minute_yield_two_directories() {
        let root = sweep_root("claim-collide");

        // Two claims microseconds apart share a clock minute unless one lands
        // across a minute boundary. Retry on a clean root rather than assert
        // against the wall clock.
        let mut pair = None;
        for _ in 0..3 {
            std::fs::remove_dir_all(&root).ok();
            std::fs::create_dir_all(&root).unwrap();
            let first = SessionDir::claim(&root, None).unwrap();
            // The first session's only copy of what was said.
            std::fs::write(first.path().join("transcript.md"), "the first conversation").unwrap();
            let second = SessionDir::claim(&root, None).unwrap();
            if second.id() == format!("{}-02", first.id()) {
                pair = Some((first, second));
                break;
            }
        }
        let (first, second) = pair.expect("two claims never landed in one clock minute");

        assert_ne!(first.path(), second.path());
        assert!(second.path().is_dir());
        // The claim is what makes the second directory empty: nothing of the
        // first's survived into it, and nothing of the first's was lost.
        assert_eq!(
            std::fs::read_to_string(first.path().join("transcript.md")).unwrap(),
            "the first conversation"
        );
        assert!(!second.path().join("transcript.md").exists());
        // The id every file inside will carry is the directory's own name.
        assert_eq!(
            second.id(),
            second.path().file_name().unwrap().to_string_lossy()
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// The suffix is zero-padded because every listing here is a byte sort.
    /// Unpadded, `-10` sorts before `-2` and the sidebar shows the tenth
    /// recording of a minute above its second.
    #[test]
    fn a_padded_suffix_sorts_between_its_minute_and_the_next() {
        let root = sweep_root("padding");
        for id in [
            "2026-08-30T1406",
            "2026-08-30T1405-10",
            "2026-08-30T1405",
            "2026-08-30T1405-02",
        ] {
            std::fs::create_dir_all(root.join(id)).unwrap();
        }
        let names: Vec<String> = list(&root)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                "2026-08-30T1405",
                "2026-08-30T1405-02",
                "2026-08-30T1405-10",
                "2026-08-30T1406",
            ]
        );
        assert_eq!(
            latest(&root).unwrap().file_name().unwrap(),
            "2026-08-30T1406"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Bug 2's other half. `app.log` sorts after every `2…` session id, so a
    /// walk without the `is_dir` filter hands back the log file as the newest
    /// session.
    #[test]
    fn list_ignores_files_in_the_sessions_folder() {
        let root = sweep_root("applog");
        fake_session(&root, "2026-01-01T0900", true);
        std::fs::write(root.join("app.log"), "some log lines\n").unwrap();
        std::fs::write(root.join("zzz.txt"), "not a session").unwrap();

        let names: Vec<String> = list(&root)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["2026-01-01T0900"]);
        assert_eq!(
            latest(&root).unwrap().file_name().unwrap(),
            "2026-01-01T0900"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// `warnings` is new, and every session recorded before it exists has a
    /// `session.json` without the field.
    #[test]
    fn a_session_json_without_warnings_still_parses() {
        let old = r#"{"id":"2026-01-01T0900","name":null,"started_at":"a","ended_at":"b",
                      "duration_s":1.0,"device_hz":48000,"mic_hz":48000,"channels":1,
                      "mic_channels":1,"apps":[],"model":"parakeet"}"#;
        let meta: SessionMeta = serde_json::from_str(old).unwrap();
        assert!(meta.warnings.is_empty());
    }

    /// The witness: a `StopSignalled` cannot be made without writing the
    /// sentinel, and the sentinel lands in the claimed directory rather than
    /// in whatever `home()` says at the time.
    #[test]
    fn signal_stop_writes_the_sentinel_into_the_claimed_directory() {
        let root = sweep_root("signalstop");
        let dir = SessionDir::claim(&root, Some("Team Sync")).unwrap();
        assert!(dir.id().ends_with("-team-sync"));
        let _witness: StopSignalled = signal_stop(&dir).unwrap();
        assert!(dir.path().join(STOP_FILE).is_file());
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

    /// What the menu bar app re-queues at launch: captured, never
    /// transcribed, and not being written by anyone. Everything else in the
    /// folder — finished, mid-transcription, killed mid-capture, or still
    /// recording in another process — is left alone.
    #[test]
    fn only_captured_untranscribed_sessions_are_awaiting_transcript() {
        let root = std::env::temp_dir().join(format!("ambient-awaiting-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let mk = |id: &str, files: &[&str]| {
            let d = root.join(id);
            std::fs::create_dir_all(d.join("audio")).unwrap();
            for f in files {
                std::fs::write(d.join(f), b"x").unwrap();
            }
            d
        };
        // Captured by the new half, never transcribed: the one to re-queue.
        let queued = mk("2026-09-02T0900", &["audio/room.wav", "session.json"]);
        // A transcriber started (raw.jsonl exists) and died before the
        // transcript: re-queued too, or the session is stuck for ever.
        let half = mk(
            "2026-09-02T1000",
            &["audio/room.wav", "session.json", "raw.jsonl"],
        );
        // Finished, audio swept.
        mk(
            "2026-09-02T1100",
            &["session.json", "raw.jsonl", "transcript.md"],
        );
        // Finished, audio still present.
        mk(
            "2026-09-02T1130",
            &[
                "audio/room.wav",
                "session.json",
                "raw.jsonl",
                "transcript.md",
            ],
        );
        // Killed mid-capture: no session.json, so not ours to guess about.
        mk("2026-09-02T1200", &["audio/room.wav"]);
        // Still being captured by another process.
        mk(
            "2026-09-02T1300",
            &["audio/room.wav", "audio/room.native.wav", "session.json"],
        );
        // Being transcribed by a live process — this one — right now.
        let busy = mk("2026-09-02T1400", &["audio/room.wav", "session.json"]);
        std::fs::write(busy.join(TRANSCRIBING_LOCK), std::process::id().to_string()).unwrap();
        // A transcriber that died holding the lock: the pid is gone, so the
        // lock is stale and the session is re-queued.
        let stale = mk("2026-09-02T1500", &["audio/room.wav", "session.json"]);
        std::fs::write(stale.join(TRANSCRIBING_LOCK), "2147483646").unwrap();

        assert_eq!(
            captured_awaiting_transcript(&root),
            vec![queued, half, stale]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Two transcribers on one session would race to truncate `raw.jsonl`.
    /// The lock refuses the second while the first is alive, and takes over
    /// from one that died.
    #[test]
    fn the_transcription_lock_refuses_a_live_holder_and_replaces_a_dead_one() {
        let dir = std::env::temp_dir().join(format!("ambient-lock-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let first = claim_transcription(&dir).unwrap();
        // Published with its contents, and the staging file is gone.
        assert_eq!(
            std::fs::read_to_string(dir.join(TRANSCRIBING_LOCK)).unwrap(),
            std::process::id().to_string()
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let refused = claim_transcription(&dir).unwrap_err().to_string();
        assert!(refused.contains("another process"), "{refused}");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        drop(first);
        assert!(!dir.join(TRANSCRIBING_LOCK).exists(), "dropping releases");

        // A lock that was taken over is not ours to remove on drop.
        std::fs::write(dir.join(TRANSCRIBING_LOCK), "2147483646").unwrap();
        let mine = TranscribeLock(dir.join(TRANSCRIBING_LOCK));
        drop(mine);
        assert!(
            dir.join(TRANSCRIBING_LOCK).exists(),
            "drop must not remove a lock naming another pid"
        );
        std::fs::remove_file(dir.join(TRANSCRIBING_LOCK)).unwrap();

        std::fs::write(dir.join(TRANSCRIBING_LOCK), "2147483646").unwrap();
        let taken = claim_transcription(&dir).expect("a dead holder's lock is stale");
        assert_eq!(
            std::fs::read_to_string(dir.join(TRANSCRIBING_LOCK)).unwrap(),
            std::process::id().to_string()
        );
        drop(taken);
        std::fs::remove_dir_all(&dir).ok();
    }
}
