//! Session-row projection and its tests, compiled only for the test runner.
use crate::session::{self, MeterPhase, SessionMeta};
use crate::state::{Live, PhaseKind};
use jiff::{civil::DateTime, fmt::strtime, tz::TimeZone, SignedDuration, Zoned};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::SystemTime;

#[allow(dead_code)]
struct Detail {
    title: String,
    subtitle: String,
    interrupted: bool,
    queued: bool,
    completed: bool,
    warnings: Vec<String>,
    has_transcript: bool,
    /// Cache key; when it moves, the rendered transcript is stale.
    stamp: SystemTime,
}

#[allow(dead_code)]
#[derive(Clone, PartialEq)]
struct LiveShot {
    id: String,
    dir: PathBuf,
    app: Option<String>,
    elapsed_s: u64,
    /// Per-interval, not the cumulative peaks `silent_tap_advice` reads: those
    /// never fall, so a meter fed from them would read healthy over a dead mic.
    room: f64,
    call: f64,
    audio_arriving: bool,
    meter: MeterPhase,
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

    fn verb(&self) -> &'static str {
        match self.meter {
            MeterPhase::Capturing => "Recording",
            MeterPhase::Transcribing => "Transcribing",
            MeterPhase::Diarizing => "Separating voices",
            MeterPhase::Done => "Finishing",
            MeterPhase::Failed => "Failed",
        }
    }

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

    /// The app is the bundle id the watcher armed on, the only name it knows.
    fn capturing_line(&self) -> String {
        // Past tense once the tap stops: the worker is busy, but nothing is
        // being captured any more.
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
        // Rewritten at each queue step, which the row's subtitle follows.
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

/// Falls back to the directory name, a `%Y-%m-%dT%H%M` stamp by construction.
fn started_at(id: &str, meta: Option<&SessionMeta>) -> Option<Zoned> {
    if let Some(t) = meta
        .and_then(|m| crate::clock::parse_rfc3339(&m.started_at))
        .map(|t| t.to_zoned(TimeZone::system()))
    {
        return Some(t);
    }
    let head = id.get(..15)?;
    strtime::parse("%Y-%m-%dT%H%M", head)
        .ok()
        .and_then(|b| b.to_datetime().ok())
        .and_then(|n: DateTime| n.to_zoned(TimeZone::system()).ok())
}

fn relative(t: Zoned) -> String {
    let fmt = |f: &str| strtime::format(f, &t).unwrap();
    let days = (Zoned::now().date() - t.date()).get_days();
    match days {
        0 => format!("Today {}", fmt("%H:%M")),
        1 => format!("Yesterday {}", fmt("%H:%M")),
        2..=6 => fmt("%a %H:%M"),
        _ => fmt("%-d %b %Y"),
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

    // `is_growing` separates a live recording from a killed one's leftover
    // wav; a missing `session.json` separates that from a finished capture.
    // The transcription lock, not `raw.jsonl`, says a job is running.
    let captured = meta.is_some();
    // The same "finished" test `sweep_audio` uses.
    let completed = dir.join("transcript.md").is_file();
    let interrupted = !captured
        && audio_present(dir)
        && !session::is_growing(&dir.join("audio").join("room.native.wav"))
        && session::live_transcriber(dir).is_none();
    let queued = captured && !completed && session::live_transcriber(dir).is_none();

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
    // `room-1` is still a diarisation label, so it is waiting for a name.
    assert!(d.subtitle.contains("1 to name"), "unnamed: {}", d.subtitle);
    assert_eq!(d.warnings, vec!["the room track is silent".to_string()]);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn audio_without_a_transcript_reads_as_interrupted() {
    let root = scratch("interrupted");
    let dir = root.join("2026-08-30T1500");
    std::fs::create_dir_all(dir.join("audio")).unwrap();
    std::fs::write(dir.join("audio").join("room.native.wav"), b"RIFF").unwrap();
    // Backdated past the ten-second window `is_growing` allows.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    filetime_set(&dir.join("audio").join("room.native.wav"), old);

    let d = describe(&dir);
    assert!(d.interrupted, "subtitle was {}", d.subtitle);
    assert!(d.subtitle.contains("Interrupted"));
    std::fs::remove_dir_all(&root).ok();
}

/// Sessions from before capture and transcription were split have no
/// `session.json` mid-transcription, which is the shape drawn here.
#[test]
fn a_session_still_transcribing_is_not_interrupted() {
    let root = scratch("transcribing");
    let dir = root.join("2026-08-30T1600");
    std::fs::create_dir_all(dir.join("audio")).unwrap();
    std::fs::write(dir.join("audio").join("room.wav"), b"RIFF").unwrap();
    std::fs::write(dir.join("audio").join("call.wav"), b"RIFF").unwrap();
    let _lock = session::claim_transcription(&dir).unwrap();

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

/// Hearing nothing is not a loss: reporting it as one sends someone looking
/// for audio that is exactly where they left it.
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

/// Naming a speaker appends to `edits.jsonl` without touching the directory's
/// own mtime.
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

#[test]
fn the_live_row_reads_as_a_recording_in_progress() {
    let (title, subtitle) = shot(252, MeterPhase::Capturing, true).row_lines();
    assert_eq!(title, "● Recording — 04:12");
    assert_eq!(subtitle, "Capturing us.zoom.xos → 2026-08-30T1412");
}

#[test]
fn a_silent_capture_says_so_in_the_row() {
    let (_, subtitle) = shot(9, MeterPhase::Capturing, false).row_lines();
    assert!(subtitle.ends_with("no audio arriving"), "{subtitle}");
}

#[test]
fn the_row_follows_the_worker_not_the_phase() {
    let (title, _) = shot(252, MeterPhase::Transcribing, true).row_lines();
    assert_eq!(title, "● Transcribing…");
    let (title, _) = shot(252, MeterPhase::Diarizing, true).row_lines();
    assert_eq!(title, "● Separating voices…");
}

#[test]
fn the_live_view_reads_the_shared_meter_and_nothing_else() {
    let root = crate::state::test_support::scratch("window-live");
    let (phase, _tx) = crate::state::test_support::recording(
        &root,
        Some("us.zoom.xos"),
        crate::state::Declined::default(),
    );
    let live = phase.live().expect("a Recording owns a Live");

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

    // The worker resets the interval peak on every write, so a quiet mic falls
    // back within a second.
    live.meter.room_peak_milli.store(0, Ordering::Relaxed);
    assert_eq!(LiveShot::of(live, phase.kind()).room, 0.0);

    std::fs::remove_dir_all(&root).ok();
}

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
    let now = Zoned::now();
    assert!(relative(now.clone()).starts_with("Today "));
    assert!(
        relative(now.checked_sub(SignedDuration::from_hours(24)).unwrap())
            .starts_with("Yesterday ")
    );
    assert!(!relative(
        now.checked_sub(SignedDuration::from_hours(24 * 400))
            .unwrap()
    )
    .contains("Today"));
}

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
