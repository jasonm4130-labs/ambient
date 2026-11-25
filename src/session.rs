//! Recording to, and reading back, an on-disk session.
//!
//! Append-only: `raw.jsonl` is what the recogniser heard and is never
//! rewritten; `edits.jsonl` layers repairs, speaker names and their reverts on
//! top, so the verbatim view never depends on the edit layer.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::time::{Duration, SystemTime};

use crate::capture::ProcessTap;
use crate::config::CaptureMode;
use crate::resample;

/// Which captured track a line came from: the room is you, the call is them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// One segment as the recogniser transcribed it. Immutable once written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawRecord {
    pub track: Track,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// Mean token probability. Parakeet invents fluent sentences from an empty
    /// room, which no upstream threshold catches. `default` loads older sessions.
    #[serde(default)]
    pub confidence: f32,
}

/// What an edit points at. Segments never overlap within a track, so the
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
    /// Capture warnings that would otherwise reach only stderr. `default` loads
    /// older sessions.
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub pinned: bool,
    /// `all` for older sessions; `mic` means there is no `call` track.
    #[serde(default)]
    pub capture: CaptureMode,
}

/// Written at capture start: `session.json` appears only at the end, and a
/// live watcher needs to know whether a call track will exist.
const CAPTURE_FILE: &str = "capture";

pub fn capture_mode(dir: &Path) -> CaptureMode {
    if let Some(m) = std::fs::read_to_string(dir.join(CAPTURE_FILE))
        .ok()
        .and_then(|t| t.parse().ok())
    {
        return m;
    }
    std::fs::read_to_string(dir.join("session.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<SessionMeta>(&t).ok())
        .map(|m| m.capture)
        .unwrap_or_default()
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    // Only an atomic store is async-signal-safe here.
    STOP.store(true, Ordering::SeqCst);
}

/// Visible on purpose, so Time Machine backs it up.
pub fn home() -> PathBuf {
    // The environment wins so a test harness never touches the user's config.
    if let Ok(p) = std::env::var("AMBIENT_HOME") {
        return PathBuf::from(p);
    }
    if let Some(p) = crate::config::Config::load().sessions_dir {
        return p;
    }
    default_home()
}

/// `~/Documents/Ambient`, where sessions live unless the settings say
/// otherwise.
pub fn default_home() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Documents").join("Ambient")
}

/// A session id names one directory directly inside the sessions folder, so
/// it can never climb out of it.
pub fn check_session_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id == "." || id == ".." || id.contains('/') {
        return Err(format!("{id:?} is not a session id"));
    }
    Ok(())
}

/// Under `open -a` the working directory is `/`, and that bundle launch is
/// the only one with the audio-capture grant, so relative paths are not enough.
pub fn models_root() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("AMBIENT_MODELS") {
        return Ok(PathBuf::from(p));
    }
    let mut candidates = vec![PathBuf::from("models")];
    if let Ok(exe) = std::env::current_exe() {
        // A sibling of Contents/MacOS, which the parent walk below never visits.
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

/// 16 kHz mono, 16-bit: what every track becomes before the helper reads it.
pub fn write_wav_16k(path: &Path, samples: &[f32]) -> Result<()> {
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

/// A file, not a signal: the LaunchServices launch that gets system audio has
/// no terminal and no pid a user can see.
pub const STOP_FILE: &str = "STOP";
/// Rewritten each second. Under `open -a` stderr goes nowhere, so this is the
/// only sign a recording is alive.
pub const STATUS_FILE: &str = "status";
/// RFC 3339 capture start, written before `session.json` exists.
pub const STARTED_FILE: &str = "started_at";

/// What the worker is doing now, for a [`Meter`]. `state.rs`'s `Phase`
/// governs which transitions are legal.
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
    pub(crate) fn from_u8(v: u8) -> MeterPhase {
        match v {
            1 => MeterPhase::Transcribing,
            2 => MeterPhase::Diarizing,
            3 => MeterPhase::Done,
            4 => MeterPhase::Failed,
            _ => MeterPhase::Capturing,
        }
    }
}

/// Per-second peaks for a live UI. Kept apart from the cumulative peaks behind
/// [`crate::capture::silent_tap_advice`], which must never reset or a dead mic
/// would read as healthy. Peaks are thousandths of full scale to fit an atomic.
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

/// A session directory this process created through [`SessionDir::claim`],
/// so no other recording writes there, even one started in the same minute.
#[derive(Debug)]
pub struct SessionDir(PathBuf);

impl SessionDir {
    /// `create_dir`, not `create_dir_all`, so the claim is atomic. Collision
    /// suffixes are zero-padded because listings sort names as bytes.
    pub fn claim(home: &Path, name: Option<&str>) -> Result<Self> {
        std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
        let started = crate::clock::now();
        let slug = name.map(slugify).filter(|s| !s.is_empty());
        let base = match &slug {
            Some(s) => format!("{}-{}", crate::clock::dir_stamp(&started), s),
            None => crate::clock::dir_stamp(&started),
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

    /// The directory's own name, including any `-02` collision suffix.
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

/// Proof that a stop was asked for. Only [`signal_stop`] can make one.
#[derive(Debug)]
pub struct StopSignalled(());

/// Unlike [`stop_recording`], addresses the claimed directory, so moving the
/// sessions folder mid-recording cannot strand the capture.
pub fn signal_stop(dir: &SessionDir) -> Result<StopSignalled> {
    let p = dir.path().join(STOP_FILE);
    std::fs::write(&p, "").with_context(|| format!("writing {}", p.display()))?;
    Ok(StopSignalled(()))
}

/// Removes a just-claimed directory unless disarmed. Disarmed once the tap
/// runs: past that point the directory may hold the only copy of a conversation.
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

/// Two transcribers on one directory would race to truncate `raw.jsonl`.
pub const TRANSCRIBING_LOCK: &str = "transcribing.lock";

/// Proof this process holds the transcription lock. Only
/// [`claim_transcription`] makes one.
#[derive(Debug)]
pub struct TranscribeLock(PathBuf);

impl Drop for TranscribeLock {
    fn drop(&mut self) {
        // A lock taken over as stale belongs to somebody else now.
        let ours = std::fs::read_to_string(&self.0)
            .is_ok_and(|s| s.trim() == std::process::id().to_string());
        if ours {
            std::fs::remove_file(&self.0).ok();
        }
    }
}

/// The lock holder's pid, if it is still alive.
pub fn live_transcriber(dir: &Path) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(dir.join(TRANSCRIBING_LOCK))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    // SAFETY: signal 0 delivers nothing; it only reports whether the pid exists.
    (unsafe { libc::kill(pid as libc::pid_t, 0) } == 0).then_some(pid)
}

/// Takes over a stale lock and refuses a live one. The pid is staged and
/// hard-linked into place, so no reader ever sees an empty lock and mistakes a
/// live claim for a stale one.
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
                    // A second AlreadyExists means someone else won the retry.
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

pub fn record(
    name: Option<&str>,
    bundles: &[String],
    seconds: Option<u64>,
    capture: Option<CaptureMode>,
) -> Result<PathBuf> {
    let dir = SessionDir::claim(&home(), name)?;
    record_into(&dir, name, bundles, seconds, capture, None)
}

/// [`capture_into`] then [`transcribe_session`], blocking.
pub fn record_into(
    dir: &SessionDir,
    name: Option<&str>,
    bundles: &[String],
    seconds: Option<u64>,
    capture: Option<CaptureMode>,
    meter: Option<std::sync::Arc<Meter>>,
) -> Result<PathBuf> {
    let path = capture_into(dir, name, bundles, seconds, capture, meter.clone())?;
    transcribe_session(&path, meter)
}

/// Capture until stopped, leaving 16 kHz `audio/room.wav` and
/// `audio/call.wav`, `session.json`, and `status` reading `captured`.
pub fn capture_into(
    dir: &SessionDir,
    name: Option<&str>,
    bundles: &[String],
    seconds: Option<u64>,
    capture: Option<CaptureMode>,
    meter: Option<std::sync::Arc<Meter>>,
) -> Result<PathBuf> {
    let mut guard = ClaimGuard(Some(dir.path()));
    let id = dir.id();
    let dir = dir.path();

    let cfg = crate::config::Config::load();
    let mode = capture.unwrap_or(cfg.capture);
    let mic_only = mode == CaptureMode::Mic;
    let bundles: &[String] = if mic_only {
        &[]
    } else if bundles.is_empty() {
        &cfg.apps
    } else {
        bundles
    };
    if mic_only {
        eprintln!("  capturing the microphone only");
    } else if !bundles.is_empty() {
        eprintln!("  capturing only: {}", bundles.join(", "));
    }

    let started = crate::clock::now();
    let audio = dir.join("audio");
    std::fs::create_dir_all(&audio).with_context(|| format!("creating {}", audio.display()))?;
    // A session that crashes mid-capture still knows when it began.
    std::fs::write(dir.join(STARTED_FILE), crate::clock::rfc3339(&started))
        .with_context(|| format!("writing {}", dir.join(STARTED_FILE).display()))?;

    let tap = if mic_only {
        ProcessTap::start_mic_only(60, cfg.input_device.as_deref())?
    } else {
        ProcessTap::start(bundles, 60, true, cfg.input_device.as_deref())?
    };
    // The mic and the tap's aggregate are separate devices with separate rates.
    let mic_hz = tap.mic_rate as u32;
    let call_hz = tap.call_rate as u32;
    let (mic_ch, call_ch) = (tap.mic_channels, tap.call_channels);
    if call_ch == 0 && mic_ch == 0 {
        bail!("capture started with no channels");
    }
    std::fs::write(dir.join(CAPTURE_FILE), format!("{}\n", mode.as_str()))
        .with_context(|| format!("writing {}", dir.join(CAPTURE_FILE).display()))?;

    if mic_only {
        eprintln!(
            "recording to {}\n  room {} Hz × {} ch, no call track — Ctrl-C, or `ambient stop`",
            dir.display(),
            mic_hz,
            mic_ch
        );
    } else {
        eprintln!(
            "recording to {}\n  room {} Hz × {} ch, call {} Hz × {} ch — Ctrl-C, or `ambient stop`",
            dir.display(),
            mic_hz,
            mic_ch,
            call_hz,
            call_ch
        );
    }

    // Native-rate scratch is the recoverable source until resampled after Stop.
    let room_native = audio.join("room.native.wav");
    let call_native = audio.join("call.native.wav");
    let spec = |hz: u32| hound::WavSpec {
        channels: 1,
        sample_rate: hz.max(1),
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut room_w = hound::WavWriter::create(&room_native, spec(mic_hz))?;
    let mut call_w = if mic_only {
        None
    } else {
        Some(hound::WavWriter::create(&call_native, spec(call_hz))?)
    };
    // Everything that can fail before this point fails with nothing recorded.
    // Everything after it may be sitting on audio, so the directory stays.
    guard.disarm();

    STOP.store(false, Ordering::SeqCst);
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
    // Ctrl-C means stop only while capturing. During transcription it kills
    // the CLI and its helper together: an abandoned job, not a counted failure.
    struct DefaultSigint;
    impl Drop for DefaultSigint {
        fn drop(&mut self) {
            unsafe {
                libc::signal(libc::SIGINT, libc::SIG_DFL);
            }
        }
    }
    let _sigint = DefaultSigint;

    let stop_file = dir.join(STOP_FILE);
    let status_file = dir.join(STATUS_FILE);
    // Never clear STOP here: the directory is freshly claimed, so any STOP is
    // a real one.
    let deadline = seconds.map(|s| std::time::Instant::now() + std::time::Duration::from_secs(s));

    let t0 = std::time::Instant::now();
    let mut drain = crate::capture::Drain::default();
    let mut frames = 0u64;
    let mut room_peak = 0.0f32;
    let mut call_peak = 0.0f32;
    let mut interval_room_peak = 0.0f32;
    let mut interval_call_peak = 0.0f32;
    let mut last_print = std::time::Instant::now();
    let mut max_rendering = 0usize;

    while !STOP.load(Ordering::SeqCst) && !stop_file.exists() {
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        // Mono, and padded for any stretch the tap slept through.
        let (room, call) = tap.drain(&mut drain);

        // Written even when nothing arrives: a silent tap is when someone is
        // watching the menu.
        if last_print.elapsed().as_millis() >= 1000 {
            max_rendering = max_rendering.max(crate::probe::processes_rendering_output());
            let secs = t0.elapsed().as_secs();
            let (mic_real, _, call_real, _) = tap.real_seconds(&drain);
            let line = if mic_real + call_real <= 0.0 {
                format!("{:02}:{:02}  no audio arriving", secs / 60, secs % 60)
            } else if mic_only {
                format!("{:02}:{:02}  room {room_peak:.3}", secs / 60, secs % 60)
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
            if let Some(w) = call_w.as_mut() {
                w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
            }
        }
    }

    room_w.finalize()?;
    if let Some(w) = call_w.take() {
        w.finalize()?;
    }
    // `frames` counts the padding `drain` inserts, so it cannot tell a stalled
    // capture from a working one.
    let (mic_real, _, call_real, call_total) = tap.real_seconds(&drain);
    drop(tap);
    std::fs::remove_file(&stop_file).ok();
    // The capture is done; the window shows "Finishing" for the resample.
    std::fs::write(&status_file, "finishing\n").ok();
    if let Some(m) = &meter {
        m.set_phase(MeterPhase::Done);
    }
    let ended = crate::clock::now();
    let duration_s = frames as f64 / mic_hz.max(call_hz).max(1) as f64;
    eprintln!("\n  stopped after {duration_s:.1}s — writing audio");
    if !mic_only && call_total - call_real > 1.0 {
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

    // Cumulative peaks: the only guard against a capture that reports success
    // and contains silence.
    let mut warnings: Vec<String> = Vec::new();
    if room_peak < crate::capture::SILENT_PEAK {
        eprintln!("  WARNING: the room track is silent — check the microphone grant.");
        warnings.push("the room track is silent — check the microphone grant.".into());
    }
    if !mic_only {
        if let Some(advice) = crate::capture::silent_tap_advice(call_peak, max_rendering, bundles) {
            eprintln!("\n  WARNING: {advice}\n");
            warnings.push(advice);
        }
    }

    // One track at a time so both native buffers are never held together. The
    // scratch goes only once its 16 kHz copy is on disk.
    for (track, native, out) in [
        (Track::Room, &room_native, audio.join("room.wav")),
        (Track::Call, &call_native, audio.join("call.wav")),
    ] {
        if track == Track::Call && mic_only {
            continue;
        }
        let (samples, rate) = resample::read_wav_any(native)?;
        let sixteen = resample::to_16k(&samples, rate)?;
        drop(samples);
        write_wav_16k(&out, &sixteen)?;
        std::fs::remove_file(native).ok();
    }

    // Before ASR, so a transcript that never arrives does not make this look
    // like a killed capture.
    let meta = SessionMeta {
        id: id.clone(),
        name: name.map(str::to_string),
        started_at: crate::clock::rfc3339(&started),
        ended_at: crate::clock::rfc3339(&ended),
        duration_s,
        device_hz: call_hz,
        mic_hz,
        channels: call_ch,
        mic_channels: mic_ch,
        apps: bundles.to_vec(),
        model: crate::pipeline::ASR_MODEL.to_string(),
        warnings,
        tags: Vec::new(),
        notes: String::new(),
        pinned: false,
        capture: mode,
    };
    std::fs::write(
        dir.join("session.json"),
        serde_json::to_string_pretty(&meta)?,
    )?;
    std::fs::write(&status_file, "captured\n").ok();
    eprintln!("  audio written — {:.1}s", duration_s);
    Ok(dir.to_path_buf())
}

/// Transcribe audio [`capture_into`] wrote, in the model helper's process
/// (ADR 0020).
pub fn transcribe_session(dir: &Path, meter: Option<std::sync::Arc<Meter>>) -> Result<PathBuf> {
    // A missing helper or model is not counted toward the retry cap, or two
    // launches without models would park every session.
    let engine = match crate::pipeline::Engine::locate().and_then(|e| e.check().map(|()| e)) {
        Ok(engine) => engine,
        Err(e) => {
            std::fs::write(dir.join(STATUS_FILE), format!("failed: {e:#}\n")).ok();
            if let Some(m) = &meter {
                m.set_phase(MeterPhase::Failed);
            }
            return Err(e);
        }
    };
    let diarize = crate::config::Config::load().diarize;
    let done = transcribe_with(dir, &engine, diarize, meter.as_ref())?;
    // Audio may age out only once its text exists.
    match sweep_audio(&home(), crate::config::Config::load().audio_retention_days) {
        0 => {}
        n => eprintln!("  {n} track(s) of aged-out audio removed"),
    }
    Ok(done)
}

/// [`transcribe_session`] with the helper given, and no audio sweep.
pub fn transcribe_with(
    dir: &Path,
    engine: &crate::pipeline::Engine,
    diarize: bool,
    meter: Option<&std::sync::Arc<Meter>>,
) -> Result<PathBuf> {
    // A refusal here is not a failure of this session's job.
    let _lock = claim_transcription(dir)?;
    // Finished while this waited for the lock.
    if dir.join("transcript.md").is_file() {
        return Ok(dir.to_path_buf());
    }
    run_locked(dir, engine, diarize, meter)
}

/// The current transcript is archived under `previous/`, never deleted, and
/// typed names carry over ([`crate::pipeline::carry_names`]).
pub fn rerun_with(
    dir: &Path,
    engine: &crate::pipeline::Engine,
    diarize: bool,
    meter: Option<&std::sync::Arc<Meter>>,
) -> Result<PathBuf> {
    let _lock = claim_transcription(dir)?;
    crate::pipeline::prepare_rerun(dir)?;
    run_locked(dir, engine, diarize, meter)
}

/// [`rerun_with`] with the located helper and the saved diarize setting.
pub fn rerun_session(dir: &Path, meter: Option<std::sync::Arc<Meter>>) -> Result<PathBuf> {
    let engine = crate::pipeline::Engine::locate()?;
    rerun_with(
        dir,
        &engine,
        crate::config::Config::load().diarize,
        meter.as_ref(),
    )
}

fn run_locked(
    dir: &Path,
    engine: &crate::pipeline::Engine,
    diarize: bool,
    meter: Option<&std::sync::Arc<Meter>>,
) -> Result<PathBuf> {
    match crate::pipeline::run(dir, engine, diarize, meter) {
        Ok(md) => {
            eprintln!("  {}", md.display());
            Ok(dir.to_path_buf())
        }
        Err(e) => {
            fail(dir, &e, meter);
            Err(e)
        }
    }
}

fn fail(dir: &Path, e: &anyhow::Error, meter: Option<&std::sync::Arc<Meter>>) {
    std::fs::write(dir.join(STATUS_FILE), format!("failed: {e:#}\n")).ok();
    crate::pipeline::note_failure(dir);
    if let Some(m) = meter {
        m.set_phase(MeterPhase::Failed);
    }
}

/// Only on a finished session: writing `transcript.md` for a pending one
/// would take it off the queue.
fn refresh_transcript(dir: &Path) -> Result<()> {
    let md = dir.join("transcript.md");
    if md.is_file() {
        std::fs::write(md, markdown(dir)?)?;
    }
    Ok(())
}

pub fn clear_warnings(dir: &Path, prefix: &str) -> Result<()> {
    let path = dir.join("session.json");
    let text = std::fs::read_to_string(&path)?;
    let mut meta: SessionMeta = serde_json::from_str(&text)?;
    let before = meta.warnings.len();
    meta.warnings.retain(|w| !w.starts_with(prefix));
    if meta.warnings.len() != before {
        let tmp = dir.join(format!(".session.json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string_pretty(&meta)?)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(())
}

fn has_warning(dir: &Path, prefix: &str) -> bool {
    std::fs::read_to_string(dir.join("session.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<SessionMeta>(&t).ok())
        .is_some_and(|m| m.warnings.iter().any(|w| w.starts_with(prefix)))
}

pub fn add_warning(dir: &Path, warning: &str) -> Result<()> {
    let path = dir.join("session.json");
    let text = std::fs::read_to_string(&path)?;
    let mut meta: SessionMeta = serde_json::from_str(&text)?;
    if !meta.warnings.iter().any(|w| w == warning) {
        meta.warnings.push(warning.to_string());
        let tmp = dir.join(format!(".session.json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string_pretty(&meta)?)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(())
}

/// Oldest first: ids are timestamps. Follows symlinks; `is_dir` keeps loose
/// files such as `app.log`, which sorts after every id, out.
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

/// Metadata fields are optional: a live capture has no `session.json` yet and
/// an interrupted one may not parse. Both are listed, never hidden.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub dir: PathBuf,
    pub name: Option<String>,
    pub started_at: Option<String>,
    pub duration_s: Option<f64>,
    pub transcribed: bool,
    pub live: bool,
    pub transcribing: bool,
    pub error: Option<String>,
    pub tags: Vec<String>,
    pub notes: String,
    pub warnings: Vec<String>,
    pub pinned: bool,
    pub audio_available: bool,
}

impl SessionSummary {
    /// `broken` outranks `done` because unreadable metadata makes it a guess.
    pub fn state(&self) -> &'static str {
        match self {
            s if s.live => "live",
            s if s.transcribing => "transcribing",
            s if s.error.is_some() => "broken",
            s if s.transcribed => "done",
            _ => "awaiting transcript",
        }
    }
}

/// Newest first, pinned first.
pub fn summaries(root: &Path) -> Vec<SessionSummary> {
    let mut out: Vec<SessionSummary> = list(root)
        .iter()
        .rev()
        .filter_map(|d| summarise(d))
        .collect();
    // Stable, so ties keep newest-first.
    out.sort_by_key(|s| !s.pinned);
    out
}

/// `None` unless `dir` holds `session.json` or a growing native wav.
pub fn summarise(dir: &Path) -> Option<SessionSummary> {
    let json = dir.join("session.json");
    // Describes a symlink itself, never its target.
    let present = std::fs::symlink_metadata(&json);
    let live = is_growing(&dir.join("audio").join("room.native.wav"));
    if present.is_err() && !live {
        return None;
    }
    let mut s = SessionSummary {
        id: dir.file_name()?.to_string_lossy().into_owned(),
        dir: dir.to_path_buf(),
        name: None,
        started_at: None,
        duration_s: None,
        transcribed: dir.join("transcript.md").is_file(),
        live,
        transcribing: live_transcriber(dir).is_some(),
        error: None,
        tags: Vec::new(),
        notes: String::new(),
        warnings: Vec::new(),
        pinned: false,
        audio_available: ["room.wav", "call.wav", "room.native.wav", "call.native.wav"]
            .iter()
            .any(|name| dir.join("audio").join(name).is_file()),
    };
    match present {
        // A live capture has not written `session.json` yet.
        Err(_) => {}
        Ok(m) if !m.is_file() => s.error = Some("session.json is a symlink".into()),
        Ok(_) => match std::fs::read_to_string(&json)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<SessionMeta>(&t).map_err(|e| e.to_string()))
        {
            Ok(meta) => {
                s.name = meta.name;
                s.started_at = Some(meta.started_at);
                s.duration_s = Some(meta.duration_s);
                s.tags = meta.tags;
                s.notes = meta.notes;
                s.warnings = meta.warnings;
                s.pinned = meta.pinned;
            }
            Err(e) => s.error = Some(format!("session.json: {e}")),
        },
    }
    Some(s)
}

/// Only `Some` fields apply. Tags change one at a time so two clients adding
/// different tags both keep theirs. No `deny_unknown_fields`: the api
/// deserialises this from params that also carry `session`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MetaPatch {
    pub name: Option<String>,
    pub notes: Option<String>,
    pub pinned: Option<bool>,
    pub add_tag: Option<String>,
    pub remove_tag: Option<String>,
}

/// Lock-free: entry points claim [`claim_transcription`] once around the
/// call, and claiming it here would collide with theirs. Staged and renamed,
/// so a crash mid-write leaves the old file intact.
pub fn update_meta(dir: &Path, patch: &MetaPatch) -> Result<SessionMeta> {
    let path = dir.join("session.json");
    let text = std::fs::read_to_string(&path).map_err(|_| {
        anyhow!(
            "{} has no session.json yet — it is still recording",
            dir.display()
        )
    })?;
    let mut meta: SessionMeta =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;

    if let Some(name) = &patch.name {
        meta.name = Some(name.clone());
    }
    if let Some(notes) = &patch.notes {
        meta.notes = notes.clone();
    }
    if let Some(pinned) = patch.pinned {
        meta.pinned = pinned;
    }
    if let Some(tag) = &patch.add_tag {
        if !meta.tags.iter().any(|t| t == tag) {
            meta.tags.push(tag.clone());
        }
    }
    if let Some(tag) = &patch.remove_tag {
        meta.tags.retain(|t| t != tag);
    }

    static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
    let tmp = dir.join(format!(
        "session.json.{}.{}.tmp",
        std::process::id(),
        NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut staging = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    let result = (|| -> Result<()> {
        staging
            .write_all(serde_json::to_string_pretty(&meta)?.as_bytes())
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("renaming {} into place", path.display()))
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result?;
    Ok(meta)
}

pub fn latest(root: &Path) -> Option<PathBuf> {
    list(root).pop()
}

/// What the menu bar app re-queues at launch. `raw.jsonl` is not consulted: a
/// transcriber that died after creating it would strand the session for ever.
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
                && crate::pipeline::attempts(dir) < crate::pipeline::RETRY_CAP
        })
        .collect()
}

/// Also the "is this a session" test: anything else has no `raw.jsonl`.
fn has_transcribed_words(session: &Path) -> bool {
    std::fs::read_to_string(session.join("raw.jsonl"))
        .is_ok_and(|t| t.lines().any(|l| !l.trim().is_empty()))
}

/// Delete track audio older than the retention window. `None` keeps it
/// forever. Age comes from the audio file, not the directory, so renaming a
/// speaker months later does not resurrect the recording.
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
        // This deletes files: never follow a link out of the sessions folder.
        if entry
            .path()
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            continue;
        }
        let session = entry.path();
        let audio = session.join("audio");
        // Each guards the only copy of a conversation: no transcript yet; a
        // transcript with no words (`markdown` writes one when ASR found
        // nothing); or one without speakers, which a person will likely rerun.
        if !session.join("transcript.md").is_file()
            || !has_transcribed_words(&session)
            || has_warning(&session, crate::pipeline::UNLABELLED_WARNING)
        {
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
        if std::fs::read_dir(&audio).is_ok_and(|mut d| d.next().is_none()) {
            std::fs::remove_dir(&audio).ok();
        }
    }
    removed
}

/// Takes the lock as proof rather than claiming it, so a caller holding it
/// cannot collide with itself. A symlink at `id` is refused, never followed,
/// and a live capture is never removed. The lock file goes with the
/// directory; [`TranscribeLock`]'s `Drop` tolerates that.
pub fn delete(root: &Path, id: &str, _lock: &TranscribeLock) -> Result<()> {
    check_session_id(id).map_err(|e| anyhow!(e))?;
    let dir = root.join(id);
    if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
        bail!("{} is not a session directory", dir.display());
    }
    let audio = dir.join("audio");
    if live_session_in(root).as_deref() == Some(dir.as_path())
        || is_growing(&audio.join("room.native.wav"))
        || is_growing(&audio.join("call.native.wav"))
    {
        bail!("{} is still recording", dir.display());
    }
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    Ok(())
}

/// `show --json` output, so field names and `room`/`call` are a contract.
#[derive(Debug, Clone, Serialize)]
pub struct Line {
    pub track: Track,
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: Option<String>,
    pub text: String,
}

/// Clock order; `verbatim` ignores the edit layer.
pub fn transcript(dir: &Path, verbatim: bool) -> Result<Vec<Line>> {
    fold_edits(dir, verbatim, true)
}

/// [`transcript`] in append order, the only order a cursor can count: the
/// tracks are written one after the other on independent clocks.
pub fn transcript_appended(dir: &Path, verbatim: bool) -> Result<Vec<Line>> {
    fold_edits(dir, verbatim, false)
}

/// `index` is the line's position in [`transcript_appended`] order.
#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub session: String,
    pub index: usize,
    pub track: Track,
    pub start_ms: u64,
    pub speaker: Option<String>,
    pub text: String,
}

/// Case-insensitive, newest session first, at most `limit` hits. Never
/// follows symlinks, and skips a session it cannot read rather than failing.
pub fn search(root: &Path, query: &str, limit: usize) -> Result<Vec<Hit>> {
    if query.trim().is_empty() {
        bail!("empty query");
    }
    let needle = query.to_lowercase();

    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(Vec::new());
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()))
        .collect();
    dirs.sort();

    let mut hits = Vec::new();
    'sessions: for dir in dirs.iter().rev() {
        let Some(id) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let not_a_regular_file = [dir.join("raw.jsonl"), dir.join("edits.jsonl")]
            .iter()
            .any(|p| std::fs::symlink_metadata(p).is_ok_and(|m| !m.is_file()));
        if not_a_regular_file {
            continue;
        }
        let Ok(lines) = transcript_appended(dir, false) else {
            continue;
        };
        for (index, line) in lines.iter().enumerate() {
            if line.text.to_lowercase().contains(&needle) {
                if hits.len() >= limit {
                    break 'sessions;
                }
                hits.push(Hit {
                    session: id.clone(),
                    index,
                    track: line.track,
                    start_ms: line.start_ms,
                    speaker: line.speaker.clone(),
                    text: line.text.clone(),
                });
            }
        }
    }
    Ok(hits)
}

fn fold_edits(dir: &Path, verbatim: bool, sort: bool) -> Result<Vec<Line>> {
    let mut lines: Vec<Line> = Vec::new();
    for l in complete_jsonl(&dir.join("raw.jsonl"))? {
        let r: RawRecord = serde_json::from_str(&l).context("parsing raw.jsonl")?;
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
            let edits: Vec<Edit> = complete_jsonl(&edits_path)?
                .into_iter()
                .map(|l| serde_json::from_str(&l).context("parsing edits.jsonl"))
                .collect::<Result<_>>()?;

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

    if sort {
        lines.sort_by_key(|l| l.start_ms);
    }
    Ok(lines)
}

/// Ignores an unterminated tail, which another thread may still be appending.
fn complete_jsonl(path: &Path) -> Result<Vec<String>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let complete = bytes
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(&bytes[..0], |last| &bytes[..=last]);
    let text = std::str::from_utf8(complete)
        .with_context(|| format!("reading {} as UTF-8", path.display()))?;
    Ok(text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect())
}

pub fn show(dir: &Path, verbatim: bool, json: bool) -> Result<()> {
    let lines = transcript(dir, verbatim)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&lines)?);
        return Ok(());
    }
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

/// A re-run carries over every speaker edit *not* by this name. Labels are
/// track-prefixed because speaker 1 in the room is not speaker 1 on the call.
pub const DIARIZE_BY: &str = "diarize";

pub const USER_BY: &str = "user";

/// Generated `<track>-<n>` labels, each with the first thing that voice said
/// so a person can recognise it.
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
            "none — voices were not separated; run `ambient rerun` with diarize on".to_string()
        } else {
            seen.join(", ")
        };
        bail!("no lines labelled {label:?} (available: {known})");
    }

    let now = crate::clock::now_rfc3339();
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
    refresh_transcript(dir)?;
    Ok(targets.len())
}

/// A line's index is its `target_seq`.
fn read_edits(dir: &Path) -> Result<Vec<Edit>> {
    let path = dir.join("edits.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).context("parsing edits.jsonl"))
        .collect()
}

fn reverted_seqs(edits: &[Edit]) -> std::collections::HashSet<usize> {
    edits
        .iter()
        .filter_map(|e| match e {
            Edit::Revert { target_seq, .. } => Some(*target_seq),
            _ => None,
        })
        .collect()
}

/// The reverts share an `at` so a batch stays legible as a batch.
fn append_reverts(dir: &Path, seqs: &[usize]) -> Result<()> {
    let now = crate::clock::now_rfc3339();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("edits.jsonl"))?;
    for &target_seq in seqs {
        let e = Edit::Revert {
            target_seq,
            by: USER_BY.into(),
            at: now.clone(),
        };
        writeln!(f, "{}", serde_json::to_string(&e)?)?;
    }
    f.flush()?;
    drop(f);
    refresh_transcript(dir)?;
    Ok(())
}

/// Reverts the newest batch of `user` namings: one [`name_speaker`] call
/// shares an `at`, and that whole batch is "the last name I set".
pub fn undo_last_naming(dir: &Path) -> Result<usize> {
    let edits = read_edits(dir)?;
    let reverted = reverted_seqs(&edits);
    let live_namings = || {
        edits
            .iter()
            .enumerate()
            .filter(|(seq, _)| !reverted.contains(seq))
            .filter_map(|(seq, e)| match e {
                Edit::Speaker { by, at, .. } if by == USER_BY => Some((seq, at.as_str())),
                _ => None,
            })
    };
    let newest = live_namings()
        .map(|(_, at)| at)
        .max()
        .ok_or_else(|| anyhow!("nothing to undo in {}", dir.display()))?
        .to_string();
    let batch: Vec<usize> = live_namings()
        .filter(|(_, at)| *at == newest)
        .map(|(seq, _)| seq)
        .collect();
    append_reverts(dir, &batch)?;
    Ok(batch.len())
}

/// Reaches what [`undo_last_naming`] does not: a repair, or one line of a batch.
pub fn undo_seq(dir: &Path, seq: usize) -> Result<()> {
    let edits = read_edits(dir)?;
    let edit = edits
        .get(seq)
        .ok_or_else(|| anyhow!("no edit {seq} in {}", dir.join("edits.jsonl").display()))?;
    // Reverting a revert would be redo, which `transcript` does not fold.
    if matches!(edit, Edit::Revert { .. }) {
        bail!("edit {seq} is itself a revert — undo the edit it names instead");
    }
    if reverted_seqs(&edits).contains(&seq) {
        bail!("edit {seq} is already reverted");
    }
    append_reverts(dir, &[seq])
}

/// Native scratch exists only from capture start until the resample.
pub fn live_session() -> Option<PathBuf> {
    live_session_in(&home())
}

pub fn live_session_in(root: &Path) -> Option<PathBuf> {
    let mut live: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| is_growing(&p.join("audio").join("room.native.wav")))
        .collect();
    live.sort();
    live.pop()
}

/// A killed recorder leaves scratch wavs behind for ever; only a recent write
/// means a capture is alive.
pub fn is_growing(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    meta.modified()
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age.as_secs() < 10)
}

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

/// The share of the shorter line's words found in the longer, 0.0–1.0.
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

/// Tolerates two ASR passes disagreeing on a word or two, yet keeps two people
/// talking over each other.
const BLEED_SIMILARITY: f32 = 0.6;

/// Drop room lines that are the mic overhearing the call. Only room lines
/// drop: the call track is the better copy (0.87 peak against the mic's 0.26)
/// and cannot be reconstructed.
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

/// Derived, never authoritative, so regenerating it loses nothing.
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

    // JSON string escaping is valid YAML double-quoted escaping.
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

    /// Temp dirs only: the sweep deletes files.
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
        let a_day_later = SystemTime::now() + Duration::from_secs(24 * 60 * 60);
        assert_eq!(sweep_audio_at(&root, Some(7), a_day_later), 0);
        assert!(dir.join("audio").join("room.wav").exists());

        let later = SystemTime::now() + Duration::from_secs(8 * 24 * 60 * 60);
        assert_eq!(sweep_audio_at(&root, Some(7), later), 2);
        assert!(!dir.join("audio").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    /// `markdown` writes a transcript even when ASR found nothing, so the file
    /// alone must not authorise deleting the only copy.
    #[test]
    fn a_transcript_with_no_words_in_it_does_not_authorise_deleting_the_audio() {
        let root = sweep_root("nowords");
        let dir = fake_session(&root, "2026-01-01T0900", true);
        std::fs::write(dir.join("raw.jsonl"), "").unwrap();
        assert_eq!(sweep_audio(&root, Some(0)), 0);
        assert!(dir.join("audio").join("room.wav").exists());
        std::fs::remove_dir_all(&root).ok();
    }

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

    #[test]
    fn a_session_without_a_transcript_is_never_swept() {
        let root = sweep_root("inflight");
        let dir = fake_session(&root, "2026-01-01T0900", false);
        assert_eq!(sweep_audio(&root, Some(0)), 0);
        assert!(dir.join("audio").join("room.wav").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn two_claims_in_one_clock_minute_yield_two_directories() {
        let root = sweep_root("claim-collide");

        // Retried in case the two claims straddle a minute boundary.
        let mut pair = None;
        for _ in 0..3 {
            std::fs::remove_dir_all(&root).ok();
            std::fs::create_dir_all(&root).unwrap();
            let first = SessionDir::claim(&root, None).unwrap();
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
        assert_eq!(
            std::fs::read_to_string(first.path().join("transcript.md")).unwrap(),
            "the first conversation"
        );
        assert!(!second.path().join("transcript.md").exists());
        assert_eq!(
            second.id(),
            second.path().file_name().unwrap().to_string_lossy()
        );

        std::fs::remove_dir_all(&root).ok();
    }

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

    #[test]
    fn a_session_json_without_warnings_still_parses() {
        let old = r#"{"id":"2026-01-01T0900","name":null,"started_at":"a","ended_at":"b",
                      "duration_s":1.0,"device_hz":48000,"mic_hz":48000,"channels":1,
                      "mic_channels":1,"apps":[],"model":"parakeet"}"#;
        let meta: SessionMeta = serde_json::from_str(old).unwrap();
        assert!(meta.warnings.is_empty());
    }

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

    /// A person named "Room-2" must not read as a generated label.
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

    /// Unguarded, a re-run would archive the transcript before finding no
    /// audio to rebuild it from.
    #[test]
    fn rerunning_a_session_whose_audio_was_swept_refuses_and_keeps_the_transcript() {
        let root = sweep_root("swept");
        let dir = session_with(&root, &[spoken_by(Track::Call, 0, "call-1", DIARIZE_BY)]);
        std::fs::write(dir.join("transcript.md"), "# t\n").unwrap();
        let before = std::fs::read_to_string(dir.join("edits.jsonl")).unwrap();

        let engine = crate::pipeline::Engine {
            binary: "/nonexistent/ambient-engine".into(),
            models: "/nonexistent/models".into(),
        };
        let e = rerun_with(&dir, &engine, true, None).unwrap_err();
        assert!(e.to_string().contains("no audio left"), "{e}");
        assert_eq!(
            std::fs::read_to_string(dir.join("edits.jsonl")).unwrap(),
            before
        );
        assert!(dir.join("transcript.md").is_file());
        assert!(!dir.join(crate::pipeline::PREVIOUS_DIR).exists());
        std::fs::remove_dir_all(&root).ok();
    }

    /// An unnamed speaker stays `null`, so a script can tell "nobody knows"
    /// from "no field".
    #[test]
    fn a_folded_transcript_serialises_as_the_documented_json() {
        let root = sweep_root("json");
        let dir = root.join("2026-01-01T0900");
        std::fs::create_dir_all(&dir).unwrap();
        let raw = [
            RawRecord {
                track: Track::Room,
                start_ms: 0,
                end_ms: 1000,
                text: "hello".into(),
                confidence: 0.9,
            },
            RawRecord {
                track: Track::Call,
                start_ms: 1000,
                end_ms: 2000,
                text: "hi".into(),
                confidence: 0.9,
            },
        ];
        let body: String = raw
            .iter()
            .map(|r| format!("{}\n", serde_json::to_string(r).unwrap()))
            .collect();
        std::fs::write(dir.join("raw.jsonl"), body).unwrap();
        std::fs::write(
            dir.join("edits.jsonl"),
            format!(
                "{}\n",
                serde_json::to_string(&spoken_by(Track::Room, 0, "Ana", USER_BY)).unwrap()
            ),
        )
        .unwrap();

        let lines = transcript(&dir, false).unwrap();
        assert_eq!(
            serde_json::to_string(&lines).unwrap(),
            r#"[{"track":"room","start_ms":0,"end_ms":1000,"speaker":"Ana","text":"hello"},{"track":"call","start_ms":1000,"end_ms":2000,"speaker":null,"text":"hi"}]"#
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn undoing_a_naming_puts_the_label_back_on_every_line_it_took() {
        let root = sweep_root("undo-batch");
        let dir = session_with(
            &root,
            &[
                spoken_by(Track::Call, 0, "SPEAKER_00", DIARIZE_BY),
                spoken_by(Track::Room, 2100, "SPEAKER_00", DIARIZE_BY),
            ],
        );
        assert_eq!(name_speaker(&dir, "SPEAKER_00", "Ana").unwrap(), 2);

        assert_eq!(undo_last_naming(&dir).unwrap(), 2);
        let lines = transcript(&dir, false).unwrap();
        assert!(
            lines
                .iter()
                .all(|l| l.speaker.as_deref() == Some("SPEAKER_00")),
            "{lines:?}"
        );

        let e = undo_last_naming(&dir).unwrap_err();
        assert!(e.to_string().contains("nothing to undo"), "{e}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn undoing_one_seq_reverts_it_once_and_then_refuses() {
        let root = sweep_root("undo-seq");
        let dir = session_with(
            &root,
            &[
                spoken_by(Track::Call, 0, "SPEAKER_00", DIARIZE_BY),
                spoken_by(Track::Room, 2100, "SPEAKER_01", DIARIZE_BY),
            ],
        );
        undo_seq(&dir, 0).unwrap();
        let lines = transcript(&dir, false).unwrap();
        assert_eq!(lines[0].speaker, None);
        assert_eq!(lines[1].speaker.as_deref(), Some("SPEAKER_01"));

        let e = undo_seq(&dir, 0).unwrap_err();
        assert!(e.to_string().contains("already reverted"), "{e}");
        let e = undo_seq(&dir, 99).unwrap_err();
        assert!(e.to_string().contains("no edit 99"), "{e}");
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

    /// Real strings: one utterance as the mic and the tap each transcribed it.
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
        let queued = mk("2026-09-02T0900", &["audio/room.wav", "session.json"]);
        // A transcriber died after creating raw.jsonl.
        let half = mk(
            "2026-09-02T1000",
            &["audio/room.wav", "session.json", "raw.jsonl"],
        );
        mk(
            "2026-09-02T1100",
            &["session.json", "raw.jsonl", "transcript.md"],
        );
        mk(
            "2026-09-02T1130",
            &[
                "audio/room.wav",
                "session.json",
                "raw.jsonl",
                "transcript.md",
            ],
        );
        mk("2026-09-02T1200", &["audio/room.wav"]);
        mk(
            "2026-09-02T1300",
            &["audio/room.wav", "audio/room.native.wav", "session.json"],
        );
        // Locked by this live process.
        let busy = mk("2026-09-02T1400", &["audio/room.wav", "session.json"]);
        std::fs::write(busy.join(TRANSCRIBING_LOCK), std::process::id().to_string()).unwrap();
        // Stale lock: no such pid.
        let stale = mk("2026-09-02T1500", &["audio/room.wav", "session.json"]);
        std::fs::write(stale.join(TRANSCRIBING_LOCK), "2147483646").unwrap();

        assert_eq!(
            captured_awaiting_transcript(&root),
            vec![queued, half, stale]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_transcription_lock_refuses_a_live_holder_and_replaces_a_dead_one() {
        let dir = std::env::temp_dir().join(format!("ambient-lock-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let first = claim_transcription(&dir).unwrap();
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

    #[test]
    fn summaries_list_every_session_including_the_unreadable_ones() {
        let root = std::env::temp_dir().join(format!("ambient-summaries-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        let mk = |id: &str| {
            let d = root.join(id);
            std::fs::create_dir_all(&d).unwrap();
            d
        };
        let meta = |id: &str| {
            format!(
                r#"{{"id":"{id}","name":"stand-up","started_at":"2026-09-05T09:00:00+01:00",
                     "ended_at":"2026-09-05T09:00:12+01:00","duration_s":12.5,"device_hz":48000,
                     "mic_hz":48000,"channels":1,"mic_channels":1,"apps":[],"model":"parakeet"}}"#
            )
        };
        let a = mk("a");
        std::fs::write(a.join("session.json"), meta("a")).unwrap();
        std::fs::write(a.join("transcript.md"), "# transcript\n").unwrap();
        let b = mk("b");
        std::fs::write(b.join("session.json"), meta("b")).unwrap();
        let c = mk("c");
        std::fs::write(c.join("session.json"), r#"{"id":"#).unwrap();
        // A capture in progress has only a growing wav.
        let d = mk("d");
        std::fs::create_dir_all(d.join("audio")).unwrap();
        std::fs::write(d.join("audio").join("room.native.wav"), b"RIFF....").unwrap();
        mk("notes");
        std::fs::write(root.join("README"), "not a session\n").unwrap();

        let got = summaries(&root);
        let ids: Vec<&str> = got.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["d", "c", "b", "a"], "newest first, nothing else");

        let by = |id: &str| got.iter().find(|s| s.id == id).unwrap();
        assert!(by("a").transcribed, "a has a transcript.md");
        assert_eq!(by("a").duration_s, Some(12.5));
        assert!(!by("b").transcribed, "b has no transcript.md");
        assert!(
            by("c")
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("session.json"),
            "the error names the file that could not be read: {:?}",
            by("c").error
        );
        assert_eq!(
            by("c").id,
            "c",
            "the directory name, when the file cannot say"
        );
        assert!(by("d").live, "d's scratch wav was written a moment ago");
        assert_eq!(by("d").error, None, "a capture in flight is not an error");
        assert_eq!(by("d").duration_s, None, "and has no duration yet");
        for id in ["a", "b", "c"] {
            assert!(!by(id).live, "{id} is not being captured");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn slugify_keeps_only_ascii_alphanumerics() {
        assert_eq!(
            slugify("Hello, World!"),
            "hello-world",
            "punctuation separates"
        );
        assert_eq!(slugify("  --x--  "), "x", "no leading or trailing dash");
        assert_eq!(slugify(""), "", "nothing in, nothing out");
        // Non-ASCII separates like punctuation; a leading one leaves no dash.
        assert_eq!(
            slugify("Ünïcode ok"),
            "n-code-ok",
            "only ASCII alphanumerics survive"
        );
    }

    #[test]
    fn mmss_is_whole_seconds_and_never_wraps() {
        assert_eq!(mmss(0), "00:00");
        assert_eq!(mmss(61_000), "01:01");
        assert_eq!(
            mmss(3_600_000),
            "60:00",
            "an hour is 60 minutes, not 01:00:00"
        );
        assert_eq!(mmss(999), "00:00", "sub-second remainders truncate");
    }

    /// The menu bar parses these exact bytes.
    #[test]
    fn status_line_pins_the_string_each_phase_shows() {
        let m = Meter::default();
        m.elapsed_ms.store(61_000, Ordering::Relaxed);
        assert_eq!(
            m.status_line(),
            "01:01  no audio arriving",
            "capturing with nothing arriving: two spaces, no levels"
        );

        m.audio_arriving.store(true, Ordering::Relaxed);
        m.room_peak_milli.store(500, Ordering::Relaxed);
        m.call_peak_milli.store(1_234, Ordering::Relaxed);
        assert_eq!(
            m.status_line(),
            "01:01 · room 0.50 · call 1.23",
            "capturing with audio: peaks are thousandths shown to two places"
        );

        m.set_phase(MeterPhase::Transcribing);
        assert_eq!(m.status_line(), "transcribing…");
        m.set_phase(MeterPhase::Diarizing);
        assert_eq!(m.status_line(), "separating voices…");
        m.set_phase(MeterPhase::Done);
        assert_eq!(m.status_line(), "done");
        m.set_phase(MeterPhase::Failed);
        assert_eq!(m.status_line(), "failed");
    }

    #[test]
    fn appended_order_is_the_file_and_transcript_is_the_clock() {
        let dir = std::env::temp_dir().join(format!("ambient-appended-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let record = |track: Track, start_ms: u64| RawRecord {
            track,
            start_ms,
            end_ms: start_ms + 1_000,
            text: format!("{} at {start_ms}", track.as_str()),
            confidence: 0.9,
        };
        std::fs::write(
            dir.join("raw.jsonl"),
            format!(
                "{}\n{}\n",
                serde_json::to_string(&record(Track::Room, 5_000)).unwrap(),
                serde_json::to_string(&record(Track::Call, 0)).unwrap()
            ),
        )
        .unwrap();

        let starts = |lines: Vec<Line>| lines.iter().map(|l| l.start_ms).collect::<Vec<_>>();
        assert_eq!(starts(transcript(&dir, false).unwrap()), vec![0, 5_000]);
        assert_eq!(
            starts(transcript_appended(&dir, false).unwrap()),
            vec![5_000, 0],
            "the call line was appended after the room line and stays there"
        );
    }

    #[test]
    fn transcript_snapshot_ignores_only_an_incomplete_appended_tail() {
        let dir =
            std::env::temp_dir().join(format!("ambient-partial-jsonl-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let complete = serde_json::to_string(&RawRecord {
            track: Track::Room,
            start_ms: 0,
            end_ms: 1_000,
            text: "complete".into(),
            confidence: 0.9,
        })
        .unwrap();
        let mut bytes = format!("{complete}\n{{\"track\":\"call\",\"text\":\"").into_bytes();
        bytes.push(0xf0); // incomplete UTF-8 from an in-flight write
        std::fs::write(dir.join("raw.jsonl"), bytes).unwrap();

        let got = transcript_appended(&dir, true).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "complete");

        std::fs::write(dir.join("raw.jsonl"), format!("{complete}\nnot-json\n")).unwrap();
        assert!(
            transcript_appended(&dir, true)
                .unwrap_err()
                .to_string()
                .contains("parsing raw.jsonl"),
            "a newline makes malformed data durable and therefore an error"
        );
        std::fs::remove_dir_all(dir).ok();
    }
}

/// Named so `cargo test session::search` selects these: libtest filters by
/// substring of the full test path.
#[cfg(test)]
mod search_tests {
    use super::*;

    fn fixture(test: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ambient-{test}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn record(track: Track, start_ms: u64, text: &str) -> RawRecord {
        RawRecord {
            track,
            start_ms,
            end_ms: start_ms + 1_000,
            text: text.into(),
            confidence: 0.9,
        }
    }

    fn write_raw(dir: &Path, lines: &[RawRecord]) {
        std::fs::create_dir_all(dir).unwrap();
        let body: String = lines
            .iter()
            .map(|l| format!("{}\n", serde_json::to_string(l).unwrap()))
            .collect();
        std::fs::write(dir.join("raw.jsonl"), body).unwrap();
    }

    fn older_session(root: &Path) -> PathBuf {
        let dir = root.join("2026-01-01T0900");
        write_raw(&dir, &[record(Track::Room, 0, "just a budget line")]);
        dir
    }

    /// Append order and clock order disagree: `Room@5000` first, `Call@0` second.
    fn newest_session(root: &Path) -> PathBuf {
        let dir = root.join("2026-02-01T0900");
        write_raw(
            &dir,
            &[
                record(Track::Room, 5_000, "Budget review at noon"),
                record(Track::Call, 0, "unrelated words"),
            ],
        );
        dir
    }

    #[test]
    fn matches_case_insensitively_newest_session_first_by_appended_index() {
        let root = fixture("search-basic");
        let newest = newest_session(&root);
        older_session(&root);

        let clock_order: Vec<String> = transcript(&newest, false)
            .unwrap()
            .iter()
            .map(|l| l.text.clone())
            .collect();
        let appended_order: Vec<String> = transcript_appended(&newest, false)
            .unwrap()
            .iter()
            .map(|l| l.text.clone())
            .collect();
        assert_ne!(clock_order, appended_order);

        let hits = search(&root, "BUDGET", 50).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].session, "2026-02-01T0900");
        assert_eq!(hits[1].session, "2026-01-01T0900");
        for hit in &hits {
            let lines = transcript_appended(&root.join(&hit.session), false).unwrap();
            assert_eq!(lines[hit.index].text, hit.text);
        }

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn no_match_is_empty() {
        let root = fixture("search-no-match");
        newest_session(&root);
        older_session(&root);

        assert!(search(&root, "zzz", 50).unwrap().is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn limit_truncates_the_result() {
        let root = fixture("search-limit");
        newest_session(&root);
        older_session(&root);

        assert_eq!(search(&root, "BUDGET", 1).unwrap().len(), 1);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_empty_query_is_refused_before_touching_the_filesystem() {
        let root = fixture("search-empty-query");
        let err = search(&root, "   ", 50).unwrap_err();
        assert!(format!("{err:#}").contains("empty query"), "{err:#}");
        std::fs::remove_dir_all(&root).ok();
    }
}

/// Named so `cargo test session::meta` selects these.
#[cfg(test)]
mod meta_tests {
    use super::*;

    #[test]
    fn metadata_update_does_not_follow_a_staging_symlink() {
        let root = fixture("meta-staging-symlink");
        let dir = root.join("session");
        write_session(&dir, "session", "2026-09-06", None);
        let outside = root.join("unrelated.txt");
        std::fs::write(&outside, "keep this").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("session.json.tmp")).unwrap();
        update_meta(
            &dir,
            &MetaPatch {
                notes: Some("new note".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep this");
        std::fs::remove_dir_all(root).unwrap();
    }

    fn fixture(test: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ambient-{test}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_session(dir: &Path, id: &str, started_at: &str, name: Option<&str>) {
        std::fs::create_dir_all(dir).unwrap();
        let meta = SessionMeta {
            id: id.into(),
            name: name.map(str::to_string),
            started_at: started_at.into(),
            ended_at: started_at.into(),
            duration_s: 42.0,
            device_hz: 48000,
            mic_hz: 48000,
            channels: 1,
            mic_channels: 1,
            apps: Vec::new(),
            model: "parakeet".into(),
            warnings: Vec::new(),
            tags: Vec::new(),
            notes: String::new(),
            pinned: false,
            capture: CaptureMode::All,
        };
        std::fs::write(
            dir.join("session.json"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn setting_the_name_leaves_duration_untouched() {
        let root = fixture("meta-name");
        let dir = root.join("2026-01-01T0900");
        write_session(&dir, "2026-01-01T0900", "2026-01-01T09:00:00+00:00", None);

        let patch = MetaPatch {
            name: Some("Stand-up".into()),
            ..Default::default()
        };
        let meta = update_meta(&dir, &patch).unwrap();
        assert_eq!(meta.name.as_deref(), Some("Stand-up"));
        assert_eq!(meta.duration_s, 42.0);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn tags_accumulate_across_calls_and_a_removal_leaves_the_rest() {
        let root = fixture("meta-tags");
        let dir = root.join("2026-01-01T0900");
        write_session(
            &dir,
            "2026-01-01T0900",
            "2026-01-01T09:00:00+00:00",
            Some("Stand-up"),
        );

        let meta = update_meta(
            &dir,
            &MetaPatch {
                add_tag: Some("1:1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(meta.name.as_deref(), Some("Stand-up"));
        assert_eq!(meta.tags, vec!["1:1".to_string()]);

        let meta = update_meta(
            &dir,
            &MetaPatch {
                add_tag: Some("budget".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(meta.name.as_deref(), Some("Stand-up"));
        assert_eq!(meta.tags, vec!["1:1".to_string(), "budget".to_string()]);

        let meta = update_meta(
            &dir,
            &MetaPatch {
                remove_tag: Some("1:1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(meta.tags, vec!["budget".to_string()]);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn pinning_the_older_session_puts_it_first() {
        let root = fixture("meta-pinned-order");
        let older = root.join("2026-01-01T0900");
        let newer = root.join("2026-02-01T0900");
        write_session(&older, "2026-01-01T0900", "2026-01-01T09:00:00+00:00", None);
        write_session(&newer, "2026-02-01T0900", "2026-02-01T09:00:00+00:00", None);

        update_meta(
            &older,
            &MetaPatch {
                pinned: Some(true),
                ..Default::default()
            },
        )
        .unwrap();

        let ids: Vec<String> = summaries(&root).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, ["2026-01-01T0900", "2026-02-01T0900"]);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_dir_with_no_session_json_is_still_recording() {
        let root = fixture("meta-no-json");
        let dir = root.join("2026-01-01T0900");
        std::fs::create_dir_all(&dir).unwrap();

        let err = update_meta(&dir, &MetaPatch::default()).unwrap_err();
        assert!(format!("{err:#}").contains("still recording"), "{err:#}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_session_json_without_the_new_keys_still_parses() {
        let old = r#"{"id":"2026-01-01T0900","name":null,"started_at":"a","ended_at":"b",
                      "duration_s":1.0,"device_hz":48000,"mic_hz":48000,"channels":1,
                      "mic_channels":1,"apps":[],"model":"parakeet"}"#;
        let meta: SessionMeta = serde_json::from_str(old).unwrap();
        assert!(meta.tags.is_empty());
        assert_eq!(meta.notes, "");
        assert!(!meta.pinned);
    }

    #[test]
    fn a_live_transcriber_lock_refuses_the_api_write_and_leaves_the_file_untouched() {
        let root = fixture("meta-lock");
        let dir = root.join("2026-01-01T0900");
        write_session(
            &dir,
            "2026-01-01T0900",
            "2026-01-01T09:00:00+00:00",
            Some("Stand-up"),
        );
        std::fs::write(dir.join(TRANSCRIBING_LOCK), std::process::id().to_string()).unwrap();
        let before = std::fs::read_to_string(dir.join("session.json")).unwrap();

        let paths = crate::api::Paths {
            config_file: root.join("config.json"),
            roster_file: root.join("roster.json"),
            sessions_root: Some(root.clone()),
        };
        let err = crate::api::call(
            "session.update",
            &serde_json::json!({"session": "2026-01-01T0900", "name": "x"}),
            &paths,
        )
        .unwrap_err();
        let message = match err {
            crate::api::ApiError::Failed(m) => m,
            other => panic!("expected Failed, got {other:?}"),
        };
        assert!(message.contains("being transcribed"), "{message}");

        let after = std::fs::read_to_string(dir.join("session.json")).unwrap();
        assert_eq!(before, after);

        std::fs::remove_dir_all(&root).ok();
    }
}

/// Named so `cargo test session::delete` selects these.
#[cfg(test)]
mod delete_tests {
    use super::*;

    fn fixture(test: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ambient-{test}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_session(dir: &Path, id: &str, started_at: &str, name: Option<&str>) {
        std::fs::create_dir_all(dir).unwrap();
        let meta = SessionMeta {
            id: id.into(),
            name: name.map(str::to_string),
            started_at: started_at.into(),
            ended_at: started_at.into(),
            duration_s: 42.0,
            device_hz: 48000,
            mic_hz: 48000,
            channels: 1,
            mic_channels: 1,
            apps: Vec::new(),
            model: "parakeet".into(),
            warnings: Vec::new(),
            tags: Vec::new(),
            notes: String::new(),
            pinned: false,
            capture: CaptureMode::All,
        };
        std::fs::write(
            dir.join("session.json"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
    }

    /// Outside [`is_growing`]'s 10 s window, far inside a 7-day retention.
    fn backdate(p: &Path) {
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_times(
            std::fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(3600)),
        )
        .unwrap();
    }

    fn write_track(dir: &Path, name: &str) -> PathBuf {
        let audio = dir.join("audio");
        std::fs::create_dir_all(&audio).unwrap();
        let p = audio.join(name);
        std::fs::write(&p, b"not really a wav").unwrap();
        p
    }

    #[test]
    fn a_finished_session_is_removed() {
        let root = fixture("delete-finished");
        let dir = root.join("2026-01-01T0900");
        write_session(&dir, "2026-01-01T0900", "2026-01-01T09:00:00+00:00", None);
        std::fs::write(dir.join("transcript.md"), "# transcript\n").unwrap();

        let lock = claim_transcription(&dir).unwrap();
        delete(&root, "2026-01-01T0900", &lock).unwrap();
        assert!(!dir.exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_failed_capture_with_no_session_json_and_stale_tracks_is_removed() {
        let root = fixture("delete-failed-capture");
        let dir = root.join("2026-01-01T0900");
        std::fs::create_dir_all(&dir).unwrap();
        backdate(&write_track(&dir, "room.native.wav"));
        backdate(&write_track(&dir, "call.native.wav"));

        let lock = claim_transcription(&dir).unwrap();
        delete(&root, "2026-01-01T0900", &lock).unwrap();
        assert!(!dir.exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_fresh_room_track_refuses() {
        let root = fixture("delete-fresh-room");
        let dir = root.join("2026-01-01T0900");
        std::fs::create_dir_all(&dir).unwrap();
        write_track(&dir, "room.native.wav");

        let lock = claim_transcription(&dir).unwrap();
        let err = delete(&root, "2026-01-01T0900", &lock).unwrap_err();
        assert!(format!("{err:#}").contains("still recording"), "{err:#}");
        assert!(dir.exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_stale_room_track_with_a_fresh_call_track_also_refuses() {
        let root = fixture("delete-fresh-call");
        let dir = root.join("2026-01-01T0900");
        std::fs::create_dir_all(&dir).unwrap();
        backdate(&write_track(&dir, "room.native.wav"));
        write_track(&dir, "call.native.wav");

        let lock = claim_transcription(&dir).unwrap();
        let err = delete(&root, "2026-01-01T0900", &lock).unwrap_err();
        assert!(format!("{err:#}").contains("still recording"), "{err:#}");
        assert!(dir.exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn dot_dot_slash_refuses() {
        let root = fixture("delete-dotdot");
        let throwaway = root.join("_lock");
        std::fs::create_dir_all(&throwaway).unwrap();
        // Id validation runs first, so a lock on an unrelated directory suffices.
        let lock = claim_transcription(&throwaway).unwrap();
        let err = delete(&root, "../x", &lock).unwrap_err();
        assert!(format!("{err:#}").contains("not a session id"), "{err:#}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_symlinked_entry_refuses() {
        let root = fixture("delete-symlink");
        let target = root.join("real-target");
        std::fs::create_dir_all(&target).unwrap();
        let link = root.join("2026-01-01T0900");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let throwaway = root.join("_lock");
        std::fs::create_dir_all(&throwaway).unwrap();
        let lock = claim_transcription(&throwaway).unwrap();
        let err = delete(&root, "2026-01-01T0900", &lock).unwrap_err();
        assert!(
            format!("{err:#}").contains("not a session directory"),
            "{err:#}"
        );
        assert!(target.exists(), "the symlink's target must survive");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_live_transcriber_lock_refuses_the_api_delete_and_leaves_the_directory() {
        let root = fixture("delete-lock");
        let dir = root.join("2026-01-01T0900");
        write_session(&dir, "2026-01-01T0900", "2026-01-01T09:00:00+00:00", None);
        std::fs::write(dir.join("transcript.md"), "# transcript\n").unwrap();
        std::fs::write(dir.join(TRANSCRIBING_LOCK), std::process::id().to_string()).unwrap();

        let paths = crate::api::Paths {
            config_file: root.join("config.json"),
            roster_file: root.join("roster.json"),
            sessions_root: Some(root.clone()),
        };
        let err = crate::api::call(
            "session.delete",
            &serde_json::json!({"session": "2026-01-01T0900"}),
            &paths,
        )
        .unwrap_err();
        let message = match err {
            crate::api::ApiError::Failed(m) => m,
            other => panic!("expected Failed, got {other:?}"),
        };
        assert!(message.contains("being transcribed"), "{message}");
        assert!(dir.exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_capture_mode_is_stored_in_the_metadata_and_defaults_to_all() {
        let mut meta: SessionMeta = serde_json::from_str(
            r#"{"id":"x","name":null,"started_at":"","ended_at":"","duration_s":1.0,
                "device_hz":48000,"channels":1,"mic_channels":1,"apps":[],"model":""}"#,
        )
        .unwrap();
        assert_eq!(meta.capture, CaptureMode::All, "an older session is `all`");
        meta.capture = CaptureMode::Mic;
        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains(r#""capture":"mic""#), "{json}");
        let back: SessionMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back.capture, CaptureMode::Mic);
    }

    #[test]
    fn capture_mode_reads_the_live_marker_then_the_metadata() {
        let dir = std::env::temp_dir().join(format!("ambient-capmode-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(capture_mode(&dir), CaptureMode::All);
        std::fs::write(dir.join("capture"), "mic\n").unwrap();
        assert_eq!(capture_mode(&dir), CaptureMode::Mic);
        std::fs::remove_file(dir.join("capture")).unwrap();
        std::fs::write(
            dir.join("session.json"),
            r#"{"id":"x","name":null,"started_at":"","ended_at":"","duration_s":1.0,
                "device_hz":0,"channels":0,"mic_channels":1,"apps":[],"model":"",
                "capture":"mic"}"#,
        )
        .unwrap();
        assert_eq!(capture_mode(&dir), CaptureMode::Mic);
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod session_id_tests {
    use super::check_session_id;

    #[test]
    fn a_session_id_cannot_leave_the_sessions_folder() {
        for bad in ["", ".", "..", "a/b", "../x", "/abs"] {
            assert!(check_session_id(bad).is_err(), "{bad:?} was accepted");
        }
        assert!(check_session_id("2026-11-25-standup").is_ok());
    }
}
