//! Pre-host-swap projection regressions, compiled only by the test runner.
use crate::session::{self, MeterPhase, SessionMeta};
use crate::state::{Live, PhaseKind};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::SystemTime;

#[allow(dead_code)]
struct Detail {
    title: String,
    subtitle: String,
    /// Audio was captured and no transcript was ever written, nothing is
    /// writing to the scratch wav now, and the run never reached the end.
    /// Out of scope to *recover* — the design says so explicitly — but not to
    /// say so.
    interrupted: bool,
    /// The audio is written and the transcript is not: the session is on the
    /// menu bar app's transcription queue, or was when the app last ran.
    queued: bool,
    /// The run reached its end: `transcript.md` is the last thing the
    /// transcriber writes, so its presence separates a session still being
    /// worked on from one that finished and simply heard nothing.
    completed: bool,
    warnings: Vec<String>,
    has_transcript: bool,
    /// What the cache is keyed on, and what tells the pane its rendered
    /// transcript has gone stale.
    stamp: SystemTime,
}

#[allow(dead_code)]
#[derive(Clone, PartialEq)]
struct LiveShot {
    id: String,
    dir: PathBuf,
    /// The call this recording belongs to, when the watcher started it.
    app: Option<String>,
    elapsed_s: u64,
    /// Per-interval peaks, 0–1. Deliberately not the cumulative peaks
    /// `silent_tap_advice` reads: those never fall, so a meter fed from them
    /// would climb once and stick, and read healthy over a dead microphone.
    room: f64,
    call: f64,
    audio_arriving: bool,
    /// What the *worker* is doing. The Rust phase governs which transitions
    /// are legal; this governs what the user is told.
    meter: MeterPhase,
    /// Whether Stop is a legal move — the Rust phase's business, not the
    /// meter's, and exactly the rule the menu's Stop item uses.
    stoppable: bool,
}

impl LiveShot {
    fn of(live: &Live, kind: PhaseKind) -> LiveShot {
        let m = &live.meter;
        let dir = live.dir.path().to_path_buf();
        LiveShot {
            id: dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            dir,
            app: live.app.clone(),
            elapsed_s: m.elapsed_ms.load(Ordering::Relaxed) / 1000,
            room: m.room_peak_milli.load(Ordering::Relaxed) as f64 / 1000.0,
            call: m.call_peak_milli.load(Ordering::Relaxed) as f64 / 1000.0,
            audio_arriving: m.audio_arriving.load(Ordering::Relaxed),
            meter: m.phase(),
            stoppable: kind == PhaseKind::Recording,
        }
    }

    fn clock(&self) -> String {
        format!("{:02}:{:02}", self.elapsed_s / 60, self.elapsed_s % 60)
    }

    /// What the worker is doing, in the words the sidebar row and the Stop
    /// button both use.
    fn verb(&self) -> &'static str {
        match self.meter {
            MeterPhase::Capturing => "Recording",
            MeterPhase::Transcribing => "Transcribing",
            MeterPhase::Diarizing => "Separating voices",
            MeterPhase::Done => "Finishing",
            MeterPhase::Failed => "Failed",
        }
    }

    /// The sidebar row's two lines. Pure, so the wording is testable without
    /// a window.
    fn row_lines(&self) -> (String, String) {
        let title = match self.meter {
            MeterPhase::Capturing => format!("● Recording — {}", self.clock()),
            _ => format!("● {}…", self.verb()),
        };
        let mut parts = vec![self.capturing_line()];
        if self.meter == MeterPhase::Capturing && !self.audio_arriving {
            parts.push("no audio arriving".into());
        }
        (title, parts.join(" · "))
    }

    /// "Capturing Zoom → 2026-08-30T1412": what is being recorded and where
    /// it is going. The app is the bundle id the watcher armed on, which is
    /// the only name the app actually knows.
    fn capturing_line(&self) -> String {
        // Past tense once the tap has stopped: the worker is still working,
        // but nothing is being captured any more, and a line still saying
        // "Capturing" would be the display lying about the microphone.
        let verb = if self.meter == MeterPhase::Capturing {
            "Capturing"
        } else {
            "Captured"
        };
        match &self.app {
            Some(app) => format!("{verb} {app} → {}", self.id),
            None => format!("Started by hand → {}", self.id),
        }
    }
}

fn stamp_of(dir: &Path) -> SystemTime {
    let mut newest = SystemTime::UNIX_EPOCH;
    for p in [
        dir.to_path_buf(),
        dir.join("raw.jsonl"),
        dir.join("edits.jsonl"),
        dir.join("session.json"),
        // Rewritten as a queued session moves from captured to transcribing
        // to done, which is what the row's subtitle follows.
        dir.join(session::STATUS_FILE),
    ] {
        if let Ok(t) = std::fs::metadata(&p).and_then(|m| m.modified()) {
            if t > newest {
                newest = t;
            }
        }
    }
    newest
}

fn meta_of(dir: &Path) -> Option<SessionMeta> {
    std::fs::read_to_string(dir.join("session.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

/// When the session started: the stored timestamp when there is one, else the
/// directory name, which is a `%Y-%m-%dT%H%M` stamp by construction.
fn started_at(id: &str, meta: Option<&SessionMeta>) -> Option<DateTime<Local>> {
    if let Some(t) = meta
        .and_then(|m| DateTime::parse_from_rfc3339(&m.started_at).ok())
        .map(|t| t.with_timezone(&Local))
    {
        return Some(t);
    }
    let head = id.get(..15)?;
    NaiveDateTime::parse_from_str(head, "%Y-%m-%dT%H%M")
        .ok()
        .and_then(|n| Local.from_local_datetime(&n).single())
}

fn relative(t: DateTime<Local>) -> String {
    let days = (Local::now().date_naive() - t.date_naive()).num_days();
    match days {
        0 => format!("Today {}", t.format("%H:%M")),
        1 => format!("Yesterday {}", t.format("%H:%M")),
        2..=6 => t.format("%a %H:%M").to_string(),
        _ => t.format("%-d %b %Y").to_string(),
    }
}

fn mmss(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

fn audio_present(dir: &Path) -> bool {
    std::fs::read_dir(dir.join("audio"))
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
}

/// Read one session well enough to draw it.
fn describe(dir: &Path) -> Detail {
    let stamp = stamp_of(dir);
    let id = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let meta = meta_of(dir);

    let lines = session::transcript(dir, false).unwrap_or_default();
    let has_transcript = !lines.is_empty();
    let mut speakers: Vec<&str> = Vec::new();
    for s in lines.iter().filter_map(|l| l.speaker.as_deref()) {
        if !speakers.contains(&s) {
            speakers.push(s);
        }
    }
    let unnamed = session::unnamed_labels(dir).map(|v| v.len()).unwrap_or(0);

    // Four things have to be true at once, and the first one is subtle.
    // `raw.jsonl` is created the instant transcription begins and filled
    // incrementally, so its *absence* is what marks a process killed
    // mid-recording. Testing for transcript *lines* instead would mark every
    // healthy session Interrupted for the whole of its own ASR pass — minutes,
    // on the happy path. `is_growing` separates a recording still in flight
    // from a killed one that left its scratch wav behind for ever; the missing
    // `session.json` separates *that* from a capture that ran to its end,
    // whose audio is safe and whose transcript is either queued or written.
    let captured = meta.is_some();
    // `transcript.md` is the last thing the transcriber writes, and the same
    // test `sweep_audio` uses for "finished".
    let completed = dir.join("transcript.md").is_file();
    let interrupted = !dir.join("raw.jsonl").exists()
        && !captured
        && audio_present(dir)
        && !session::is_growing(&dir.join("audio").join("room.native.wav"));
    // Captured and not yet claimed by a transcriber: waiting on the queue.
    let queued = captured && !dir.join("raw.jsonl").exists() && !completed;

    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = started_at(&id, meta.as_ref()) {
        parts.push(relative(t));
    }
    if let Some(m) = &meta {
        parts.push(mmss(m.duration_s));
    }
    if interrupted {
        parts.push("Interrupted".into());
    } else if queued {
        parts.push("waiting to transcribe".into());
    } else if !has_transcript && completed {
        parts.push("no speech recognised".into());
    } else if !has_transcript {
        parts.push("no transcript yet".into());
    }
    match speakers.len() {
        0 => {}
        1 => parts.push("1 speaker".into()),
        n => parts.push(format!("{n} speakers")),
    }
    if unnamed > 0 {
        parts.push(format!("{unnamed} to name"));
    }

    Detail {
        title: meta
            .as_ref()
            .and_then(|m| m.name.clone())
            .unwrap_or_else(|| id.clone()),
        subtitle: parts.join(" · "),
        interrupted,
        queued,
        completed,
        warnings: meta.map(|m| m.warnings).unwrap_or_default(),
        has_transcript,
        stamp,
    }
}

fn scratch(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("ambient-window-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&p).ok();
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// The row's second line is the whole of the sidebar's information, and
/// every part of it is derived rather than stored.
#[test]
fn a_transcribed_session_says_when_how_long_and_who() {
    let root = scratch("described");
    let dir = root.join("2026-08-30T1412");
    std::fs::create_dir_all(dir.join("audio")).unwrap();
    std::fs::write(
            dir.join("raw.jsonl"),
            "{\"track\":\"room\",\"start_ms\":0,\"end_ms\":2000,\"text\":\"hello\",\"confidence\":0.9}\n",
        )
        .unwrap();
    std::fs::write(
            dir.join("edits.jsonl"),
            "{\"kind\":\"speaker\",\"target\":{\"track\":\"room\",\"start_ms\":0},\"name\":\"room-1\",\"by\":\"diarize\",\"at\":\"2026-08-30T14:13:00+10:00\"}\n",
        )
        .unwrap();
    std::fs::write(
            dir.join("session.json"),
            "{\"id\":\"2026-08-30T1412\",\"name\":null,\"started_at\":\"2026-08-30T14:12:00+10:00\",\"ended_at\":\"2026-08-30T14:13:05+10:00\",\"duration_s\":65.0,\"device_hz\":48000,\"channels\":1,\"mic_channels\":1,\"apps\":[],\"model\":\"m\",\"warnings\":[\"the room track is silent\"]}",
        )
        .unwrap();

    let d = describe(&dir);
    assert_eq!(d.title, "2026-08-30T1412");
    assert!(d.has_transcript);
    assert!(!d.interrupted);
    assert!(d.subtitle.contains("01:05"), "duration: {}", d.subtitle);
    assert!(d.subtitle.contains("1 speaker"), "speakers: {}", d.subtitle);
    // `room-1` still has diarization's shape, so it is a label waiting for
    // a name — the badge the whole naming step hangs off.
    assert!(d.subtitle.contains("1 to name"), "unnamed: {}", d.subtitle);
    assert_eq!(d.warnings, vec!["the room track is silent".to_string()]);
    std::fs::remove_dir_all(&root).ok();
}

/// Audio, no transcript, nothing writing: a process killed mid-recording.
#[test]
fn audio_without_a_transcript_reads_as_interrupted() {
    let root = scratch("interrupted");
    let dir = root.join("2026-08-30T1500");
    std::fs::create_dir_all(dir.join("audio")).unwrap();
    std::fs::write(dir.join("audio").join("room.native.wav"), b"RIFF").unwrap();
    // Backdate it past the ten-second window `is_growing` allows.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    filetime_set(&dir.join("audio").join("room.native.wav"), old);

    let d = describe(&dir);
    assert!(d.interrupted, "subtitle was {}", d.subtitle);
    assert!(d.subtitle.contains("Interrupted"));
    std::fs::remove_dir_all(&root).ok();
}

/// Mid-transcription, which is the state every healthy session passes
/// through for the minutes ASR and diarization take: audio, a `raw.jsonl`
/// created the instant the transcriber claimed the session, no growing
/// wav and not one transcript line. Keying `interrupted` off transcript
/// lines rather than off `raw.jsonl` marked all of it Interrupted — in
/// the very pane this step exists to make honest. Sessions from before
/// capture and transcription were split have no `session.json` at this
/// point either, which is the shape drawn here.
#[test]
fn a_session_still_transcribing_is_not_interrupted() {
    let root = scratch("transcribing");
    let dir = root.join("2026-08-30T1600");
    std::fs::create_dir_all(dir.join("audio")).unwrap();
    // The finished tracks survive; the native scratch wavs are already gone.
    std::fs::write(dir.join("audio").join("room.wav"), b"RIFF").unwrap();
    std::fs::write(dir.join("audio").join("call.wav"), b"RIFF").unwrap();
    // Created the instant transcription begins, and empty until the first
    // segment.
    std::fs::write(dir.join("raw.jsonl"), b"").unwrap();

    let d = describe(&dir);
    assert!(
        !d.interrupted,
        "a session mid-transcription must not read as interrupted; subtitle was {}",
        d.subtitle
    );
    assert!(!d.subtitle.contains("Interrupted"), "{}", d.subtitle);
    std::fs::remove_dir_all(&root).ok();
}

const META: &str = "{\"id\":\"2026-08-30T2000\",\"name\":null,\"started_at\":\"2026-08-30T20:00:00+10:00\",\"ended_at\":\"2026-08-30T20:00:20+10:00\",\"duration_s\":20.0,\"device_hz\":48000,\"channels\":1,\"mic_channels\":1,\"apps\":[],\"model\":\"m\"}";

/// A run that reached its end wrote `transcript.md`. It heard nothing, and
/// that is a different fact from a capture that was killed — reporting it
/// as a loss would send someone looking for audio that is exactly where
/// they left it.
#[test]
fn a_finished_recording_that_heard_nothing_is_not_interrupted() {
    let root = scratch("silent");
    let dir = root.join("2026-08-30T2000");
    std::fs::create_dir_all(dir.join("audio")).unwrap();
    std::fs::write(dir.join("audio").join("room.wav"), b"RIFF").unwrap();
    std::fs::write(dir.join("raw.jsonl"), b"").unwrap();
    std::fs::write(dir.join("session.json"), META).unwrap();
    std::fs::write(dir.join("transcript.md"), "# nothing\n").unwrap();

    let d = describe(&dir);
    assert!(!d.interrupted, "subtitle was {}", d.subtitle);
    assert!(d.completed);
    assert!(!d.queued);
    assert!(
        d.subtitle.contains("no speech recognised"),
        "{}",
        d.subtitle
    );
    std::fs::remove_dir_all(&root).ok();
}

/// The state the capture/transcription split created: audio and
/// `session.json` written, no `raw.jsonl` because no transcriber has
/// claimed it yet. Neither a loss nor a silent recording — it is waiting
/// on the queue, and must say so.
#[test]
fn a_captured_session_awaiting_its_transcript_says_so() {
    let root = scratch("queued");
    let dir = root.join("2026-08-30T2000");
    std::fs::create_dir_all(dir.join("audio")).unwrap();
    std::fs::write(dir.join("audio").join("room.wav"), b"RIFF").unwrap();
    std::fs::write(dir.join("session.json"), META).unwrap();
    std::fs::write(dir.join("status"), "captured\n").unwrap();

    let d = describe(&dir);
    assert!(d.queued);
    assert!(!d.interrupted, "subtitle was {}", d.subtitle);
    assert!(!d.completed);
    assert!(
        d.subtitle.contains("waiting to transcribe"),
        "{}",
        d.subtitle
    );
    std::fs::remove_dir_all(&root).ok();
}

/// An empty directory is not a corpse — nothing was captured — so it must
/// not be labelled as one.
#[test]
fn an_empty_directory_is_not_interrupted() {
    let root = scratch("empty");
    let dir = root.join("2026-08-30T1600");
    std::fs::create_dir_all(&dir).unwrap();
    let d = describe(&dir);
    assert!(!d.interrupted);
    assert!(d.subtitle.contains("no transcript yet"));
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_named_session_is_titled_by_its_name() {
    let root = scratch("named");
    let dir = root.join("2026-08-30T1700");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
            dir.join("session.json"),
            "{\"id\":\"2026-08-30T1700\",\"name\":\"Export spec\",\"started_at\":\"2026-08-30T17:00:00+10:00\",\"ended_at\":\"2026-08-30T17:01:00+10:00\",\"duration_s\":60.0,\"device_hz\":48000,\"channels\":1,\"mic_channels\":1,\"apps\":[],\"model\":\"m\"}",
        )
        .unwrap();
    assert_eq!(describe(&dir).title, "Export spec");
    std::fs::remove_dir_all(&root).ok();
}

/// Old `session.json` files have no `warnings` key, and must still load.
#[test]
fn a_session_written_before_warnings_existed_still_parses() {
    let root = scratch("nowarnings");
    let dir = root.join("2026-08-30T1800");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
            dir.join("session.json"),
            "{\"id\":\"2026-08-30T1800\",\"name\":null,\"started_at\":\"2026-08-30T18:00:00+10:00\",\"ended_at\":\"2026-08-30T18:00:30+10:00\",\"duration_s\":30.0,\"device_hz\":48000,\"channels\":1,\"mic_channels\":1,\"apps\":[],\"model\":\"m\"}",
        )
        .unwrap();
    assert!(describe(&dir).warnings.is_empty());
    std::fs::remove_dir_all(&root).ok();
}

/// The cache key has to notice an append to `edits.jsonl`, because naming
/// a speaker is an append and does not touch the directory's own mtime.
#[test]
fn the_stamp_moves_when_edits_are_appended() {
    let root = scratch("stamp");
    let dir = root.join("2026-08-30T1900");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("edits.jsonl"), b"a\n").unwrap();
    let before = stamp_of(&dir);
    filetime_set(
        &dir.join("edits.jsonl"),
        std::time::SystemTime::now() + std::time::Duration::from_secs(5),
    );
    assert!(stamp_of(&dir) > before);
    std::fs::remove_dir_all(&root).ok();
}

fn shot(elapsed_s: u64, meter: MeterPhase, arriving: bool) -> LiveShot {
    LiveShot {
        id: "2026-08-30T1412".into(),
        dir: PathBuf::from("/tmp/2026-08-30T1412"),
        app: Some("us.zoom.xos".into()),
        elapsed_s,
        room: 0.31,
        call: 0.87,
        audio_arriving: arriving,
        meter,
        stoppable: meter == MeterPhase::Capturing,
    }
}

/// The pinned row says what is being recorded, where it is going, and how
/// long it has been going for.
#[test]
fn the_live_row_reads_as_a_recording_in_progress() {
    let (title, subtitle) = shot(252, MeterPhase::Capturing, true).row_lines();
    assert_eq!(title, "● Recording — 04:12");
    assert_eq!(subtitle, "Capturing us.zoom.xos → 2026-08-30T1412");
}

/// The one failure this app cannot afford to be quiet about: a denied tap
/// returns correctly-shaped zeros rather than an error, so "nothing is
/// arriving" is the only thing that separates it from a quiet room.
#[test]
fn a_silent_capture_says_so_in_the_row() {
    let (_, subtitle) = shot(9, MeterPhase::Capturing, false).row_lines();
    assert!(subtitle.ends_with("no audio arriving"), "{subtitle}");
}

/// Once the worker moves on, the clock stops being the news — and a row
/// still saying "Recording" over a capture that has finished would be the
/// display lying about the worker, which is what the meter exists to stop.
#[test]
fn the_row_follows_the_worker_not_the_phase() {
    let (title, _) = shot(252, MeterPhase::Transcribing, true).row_lines();
    assert_eq!(title, "● Transcribing…");
    let (title, _) = shot(252, MeterPhase::Diarizing, true).row_lines();
    assert_eq!(title, "● Separating voices…");
}

/// The whole point of step 4's per-interval peaks: the live view reads
/// those, not the cumulative ones `silent_tap_advice` guards the capture
/// with, and not the filesystem.
#[test]
fn the_live_view_reads_the_shared_meter_and_nothing_else() {
    let root = crate::state::test_support::scratch("window-live");
    let (phase, _tx) = crate::state::test_support::recording(
        &root,
        Some("us.zoom.xos"),
        crate::state::Declined::default(),
    );
    let live = phase.live().expect("a Recording owns a Live");

    // Exactly what the worker thread writes once a second.
    live.meter.elapsed_ms.store(65_000, Ordering::Relaxed);
    live.meter.room_peak_milli.store(310, Ordering::Relaxed);
    live.meter.call_peak_milli.store(870, Ordering::Relaxed);
    live.meter.audio_arriving.store(true, Ordering::Relaxed);

    let shot = LiveShot::of(live, phase.kind());
    assert_eq!(shot.clock(), "01:05");
    assert!((shot.room - 0.31).abs() < 1e-9, "room: {}", shot.room);
    assert!((shot.call - 0.87).abs() < 1e-9, "call: {}", shot.call);
    assert!(
        shot.stoppable,
        "a Recording phase is the one Stop is legal in"
    );
    assert_eq!(
        shot.id,
        live.dir.path().file_name().unwrap().to_string_lossy()
    );

    // A mic that goes quiet falls back within a second, because the worker
    // resets the interval peak every time it writes one. Reading the
    // cumulative peaks here would hold 0.31 for the rest of the capture.
    live.meter.room_peak_milli.store(0, Ordering::Relaxed);
    assert_eq!(LiveShot::of(live, phase.kind()).room, 0.0);

    std::fs::remove_dir_all(&root).ok();
}

/// Stop is the phase's business, the label is the meter's. A phase that
/// has left `Recording` must not offer a Stop that would do nothing.
#[test]
fn stop_is_offered_only_while_the_phase_is_recording() {
    let root = crate::state::test_support::scratch("window-stop");
    let (phase, _tx) =
        crate::state::test_support::recording(&root, None, crate::state::Declined::default());
    let stopped = phase.stop().unwrap_or_else(|(_, e)| panic!("{e}"));
    let live = stopped.live().unwrap();
    assert!(!LiveShot::of(live, stopped.kind()).stoppable);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn relative_dates_read_like_dates() {
    let now = Local::now();
    assert!(relative(now).starts_with("Today "));
    assert!(relative(now - chrono::Duration::days(1)).starts_with("Yesterday "));
    assert!(!relative(now - chrono::Duration::days(400)).contains("Today"));
}

/// `libc::utimes` rather than a dev-dependency: one call, and the tests
/// above need nothing else from it.
fn filetime_set(p: &Path, t: SystemTime) {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as libc::time_t;
    let tv = libc::timeval {
        tv_sec: secs,
        tv_usec: 0,
    };
    let times = [tv, tv];
    let c = std::ffi::CString::new(p.to_string_lossy().as_bytes()).unwrap();
    unsafe {
        libc::utimes(c.as_ptr(), times.as_ptr());
    }
}
