//! The transcription job against a stub `ambient-engine`
//! (tests/support/stub-engine) that fails or dies at a named stage.

use std::path::{Path, PathBuf};

use ambient::pipeline::{self, Engine, ATTEMPTS_FILE, CARRY_FILE, PREVIOUS_DIR, RETRY_CAP};
use ambient::session::{self, Edit, RawRecord, STATUS_FILE, USER_BY};

const ID: &str = "2026-11-25T09-00-00";

struct Fixture {
    root: PathBuf,
    engine: Engine,
}

impl Fixture {
    fn new(test: &str) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("ambient-pipeline-{test}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let dir = root.join("sessions").join(ID);
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        std::fs::write(
            dir.join("session.json"),
            format!(
                r#"{{"id":"{ID}","name":null,"started_at":"2026-11-25T09:00:00+00:00",
                "ended_at":"2026-11-25T09:01:00+00:00","duration_s":12.0,"device_hz":16000,
                "channels":1,"mic_channels":1,"apps":[],"model":"stand-in","warnings":[]}}"#
            ),
        )
        .unwrap();
        for track in ["room", "call"] {
            write_wav(&dir.join("audio").join(format!("{track}.wav")), 160);
        }
        std::fs::write(dir.join(STATUS_FILE), "captured\n").unwrap();
        let engine = Engine {
            binary: Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/stub-engine"),
            models: root.join("models"),
        };
        Fixture { root, engine }
    }

    fn dir(&self) -> PathBuf {
        self.root.join("sessions").join(ID)
    }

    fn steer(&self, what: &str, stage: &str) {
        std::fs::write(self.dir().join(format!("stub-{what}-{stage}")), "").unwrap();
    }

    fn unsteer(&self, what: &str, stage: &str) {
        std::fs::remove_file(self.dir().join(format!("stub-{what}-{stage}"))).unwrap();
    }

    /// Stages the stub ran, in order; clears the record.
    fn calls(&self) -> Vec<String> {
        let path = self.dir().join("stub-calls");
        let calls = std::fs::read_to_string(&path).unwrap_or_default();
        std::fs::remove_file(&path).ok();
        calls.lines().map(str::to_string).collect()
    }

    fn transcribe(&self) -> anyhow::Result<PathBuf> {
        session::transcribe_with(&self.dir(), &self.engine, true, None)
    }

    fn status(&self) -> String {
        std::fs::read_to_string(self.dir().join(STATUS_FILE)).unwrap_or_default()
    }

    fn requeued(&self) -> bool {
        session::captured_awaiting_transcript(&self.root.join("sessions")).contains(&self.dir())
    }

    fn finished(&self) -> bool {
        self.dir().join("transcript.md").is_file()
    }

    fn raw(&self) -> Vec<RawRecord> {
        jsonl(&self.dir().join("raw.jsonl"))
    }

    fn edits(&self) -> Vec<Edit> {
        jsonl(&self.dir().join("edits.jsonl"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

fn write_wav(path: &Path, samples: usize) {
    let mut wav = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for _ in 0..samples {
        wav.write_sample(0i16).unwrap();
    }
    wav.finalize().unwrap();
}

fn jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn speaker_names(edits: &[Edit]) -> Vec<(String, u64, String, String)> {
    edits
        .iter()
        .filter_map(|e| match e {
            Edit::Speaker {
                target, name, by, ..
            } => Some((
                target.track.as_str().to_string(),
                target.start_ms,
                name.clone(),
                by.clone(),
            )),
            _ => None,
        })
        .collect()
}

#[test]
fn a_session_is_transcribed_and_labelled_by_track() {
    let f = Fixture::new("happy");
    f.transcribe().unwrap();
    assert!(f.finished());
    assert_eq!(f.status(), "done\n");
    assert_eq!(
        f.calls(),
        [
            "transcribe room.wav",
            "transcribe call.wav",
            "diarize room.wav",
            "diarize call.wav"
        ]
    );
    let raw = f.raw();
    assert_eq!(
        raw.iter()
            .map(|r| (r.track.as_str(), r.start_ms, r.text.as_str()))
            .collect::<Vec<_>>(),
        [
            ("room", 0, "Hello there."),
            ("room", 6000, "Reply here."),
            ("call", 0, "Hello there."),
            ("call", 6000, "Reply here."),
        ]
    );
    let names = speaker_names(&f.edits());
    assert_eq!(
        names.iter().map(|n| n.2.as_str()).collect::<Vec<_>>(),
        ["room-1", "room-2", "call-1", "call-2"]
    );
    assert!(!f.requeued());
    assert!(!f.dir().join(ATTEMPTS_FILE).exists());
}

#[test]
fn a_failed_diarize_resumes_from_the_transcribe_checkpoint() {
    let f = Fixture::new("resume");
    f.steer("fail", "diarize");
    let e = f.transcribe().unwrap_err();
    assert!(
        format!("{e:#}").contains("told to fail at diarize"),
        "{e:#}"
    );
    assert!(f.status().starts_with("failed: "), "{}", f.status());
    assert!(!f.finished());
    assert!(f.requeued(), "one failure is under the cap");
    assert_eq!(pipeline::attempts(&f.dir()), 1);
    f.calls();

    f.unsteer("fail", "diarize");
    f.transcribe().unwrap();
    assert_eq!(f.calls(), ["diarize room.wav", "diarize call.wav"]);
    assert!(f.finished());
    assert!(
        !f.dir().join(ATTEMPTS_FILE).exists(),
        "success clears the count"
    );
}

#[test]
fn a_helper_killed_by_a_signal_is_a_counted_failure() {
    let f = Fixture::new("crash");
    f.steer("crash", "transcribe");
    let e = f.transcribe().unwrap_err();
    assert!(format!("{e:#}").contains("killed by signal 9"), "{e:#}");
    assert_eq!(pipeline::attempts(&f.dir()), 1);
    assert!(!f.dir().join("raw.jsonl").exists());
}

#[test]
fn after_two_failures_a_session_waits_for_retry() {
    let f = Fixture::new("cap");
    f.steer("fail", "transcribe");
    for _ in 0..RETRY_CAP {
        f.transcribe().unwrap_err();
    }
    assert_eq!(pipeline::attempts(&f.dir()), RETRY_CAP);
    assert!(
        !f.requeued(),
        "at the cap the app must not re-queue it at launch"
    );

    pipeline::reset_attempts(&f.dir());
    assert!(f.requeued());
    f.unsteer("fail", "transcribe");
    f.transcribe().unwrap();
    assert!(f.finished());
}

#[test]
fn at_the_cap_a_diariser_fault_gives_way_to_an_unlabelled_transcript() {
    let f = Fixture::new("unlabelled");
    f.steer("fail", "diarize");
    f.transcribe().unwrap_err();
    // The attempt that would reach the cap writes the words instead.
    f.transcribe().unwrap();
    assert!(f.finished());
    assert_eq!(f.raw().len(), 4);
    assert!(speaker_names(&f.edits()).is_empty());
    let meta = std::fs::read_to_string(f.dir().join("session.json")).unwrap();
    assert!(meta.contains("voices were not separated"), "{meta}");
}

#[test]
fn changed_audio_invalidates_its_checkpoints() {
    let f = Fixture::new("key");
    f.transcribe().unwrap();
    f.calls();
    // A different length is a different key whatever the clock says.
    write_wav(&f.dir().join("audio").join("room.wav"), 320);
    std::fs::remove_file(f.dir().join("transcript.md")).unwrap();
    f.transcribe().unwrap();
    assert_eq!(f.calls(), ["transcribe room.wav", "diarize room.wav"]);
}

#[test]
fn a_crash_after_the_lines_but_before_the_transcript_recovers_cleanly() {
    let f = Fixture::new("between");
    f.transcribe().unwrap();
    // The state a crash between `edits.jsonl` and `transcript.md` leaves.
    std::fs::remove_file(f.dir().join("transcript.md")).unwrap();
    assert!(f.requeued());
    f.transcribe().unwrap();
    assert!(f.finished());
    assert_eq!(f.raw().len(), 4);
    assert!(!f.dir().join(CARRY_FILE).exists());
}

#[test]
fn a_rerun_archives_the_old_transcript_and_carries_typed_names() {
    let f = Fixture::new("rerun");
    f.transcribe().unwrap();
    session::name_speaker(&f.dir(), "call-2", "Priya").unwrap();
    let before = std::fs::read_to_string(f.dir().join("edits.jsonl")).unwrap();
    f.calls();

    session::rerun_with(&f.dir(), &f.engine, true, None).unwrap();
    assert!(
        f.calls().is_empty(),
        "same audio, same models: every stage is a checkpoint"
    );
    assert!(f.finished());

    let archives: Vec<_> = std::fs::read_dir(f.dir().join(PREVIOUS_DIR))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(archives.len(), 1);
    assert_eq!(
        std::fs::read_to_string(archives[0].join("edits.jsonl")).unwrap(),
        before
    );
    assert!(archives[0].join("transcript.md").is_file());

    let typed: Vec<_> = speaker_names(&f.edits())
        .into_iter()
        .filter(|n| n.3 == USER_BY)
        .collect();
    assert_eq!(
        typed,
        [(
            "call".to_string(),
            6000,
            "Priya".to_string(),
            USER_BY.to_string()
        )]
    );
    assert!(!f.dir().join(CARRY_FILE).exists());
}

#[test]
fn a_rerun_that_dies_still_carries_names_when_it_resumes() {
    let f = Fixture::new("rerun-crash");
    f.transcribe().unwrap();
    session::name_speaker(&f.dir(), "room-1", "Ana").unwrap();
    // Force the stages to rerun, failing the first.
    std::fs::remove_dir_all(f.dir().join("stages")).unwrap();
    f.steer("fail", "transcribe");
    session::rerun_with(&f.dir(), &f.engine, true, None).unwrap_err();
    assert!(!f.finished());
    assert!(f.dir().join(CARRY_FILE).exists());
    assert!(f.requeued());

    f.unsteer("fail", "transcribe");
    f.transcribe().unwrap();
    let typed: Vec<_> = speaker_names(&f.edits())
        .into_iter()
        .filter(|n| n.3 == USER_BY)
        .map(|n| (n.0, n.1, n.2))
        .collect();
    assert_eq!(typed, [("room".to_string(), 0, "Ana".to_string())]);
    let archives = std::fs::read_dir(f.dir().join(PREVIOUS_DIR))
        .unwrap()
        .count();
    assert_eq!(
        archives, 1,
        "the resumed run must not archive its own half-state"
    );
}

#[test]
fn a_second_transcriber_is_refused_without_counting_a_failure() {
    let f = Fixture::new("locked");
    let _held = session::claim_transcription(&f.dir()).unwrap();
    // The lock names this process, which is alive, so a second claim from a
    // different code path must still be refused.
    let e = f.transcribe();
    assert!(e.is_err());
    assert_eq!(pipeline::attempts(&f.dir()), 0);
}

#[test]
fn a_name_typed_while_a_failed_rerun_waits_is_kept_and_does_not_finish_it() {
    let f = Fixture::new("rerun-named-while-waiting");
    f.transcribe().unwrap();
    session::name_speaker(&f.dir(), "room-1", "Ana").unwrap();
    std::fs::remove_dir_all(f.dir().join("stages")).unwrap();
    f.steer("fail", "transcribe");
    session::rerun_with(&f.dir(), &f.engine, true, None).unwrap_err();

    // The old lines are still on screen, and someone names another speaker.
    session::name_speaker(&f.dir(), "call-2", "Priya").unwrap();
    assert!(
        !f.finished(),
        "naming must not mark a waiting session finished"
    );
    assert!(f.requeued());

    f.unsteer("fail", "transcribe");
    f.transcribe().unwrap();
    let mut typed: Vec<_> = speaker_names(&f.edits())
        .into_iter()
        .filter(|n| n.3 == USER_BY)
        .map(|n| n.2)
        .collect();
    typed.sort();
    assert_eq!(typed, ["Ana", "Priya"]);
    let archives = std::fs::read_dir(f.dir().join(PREVIOUS_DIR))
        .unwrap()
        .count();
    assert_eq!(
        archives, 2,
        "the edits typed while waiting were archived too"
    );
}

#[test]
fn transcribing_a_finished_session_does_nothing() {
    let f = Fixture::new("already-done");
    f.transcribe().unwrap();
    session::name_speaker(&f.dir(), "room-1", "Ana").unwrap();
    let edits = std::fs::read_to_string(f.dir().join("edits.jsonl")).unwrap();
    f.calls();
    f.transcribe().unwrap();
    assert!(f.calls().is_empty());
    assert_eq!(
        std::fs::read_to_string(f.dir().join("edits.jsonl")).unwrap(),
        edits
    );
    assert!(!f.dir().join(PREVIOUS_DIR).exists());
}

#[test]
fn an_unlabelled_fallback_keeps_names_its_audio_and_clears_its_warning_later() {
    let f = Fixture::new("fallback-carry");
    f.transcribe().unwrap();
    session::name_speaker(&f.dir(), "call-2", "Priya").unwrap();
    std::fs::remove_dir_all(f.dir().join("stages")).unwrap();

    // The rerun's diariser fails twice; the second time falls back.
    f.steer("fail", "diarize");
    session::rerun_with(&f.dir(), &f.engine, true, None).unwrap_err();
    f.transcribe().unwrap();
    assert!(f.finished());
    assert!(speaker_names(&f.edits()).is_empty());
    assert!(
        f.dir().join(CARRY_FILE).exists(),
        "nothing was carried, so the names wait for a labelled run"
    );
    // The fallback asks for another run, which needs the audio.
    let sessions = f.root.join("sessions");
    assert_eq!(
        session::sweep_audio_at(&sessions, Some(0), std::time::SystemTime::now()),
        0
    );

    f.unsteer("fail", "diarize");
    session::rerun_with(&f.dir(), &f.engine, true, None).unwrap();
    let typed: Vec<_> = speaker_names(&f.edits())
        .into_iter()
        .filter(|n| n.3 == USER_BY)
        .map(|n| n.2)
        .collect();
    assert_eq!(typed, ["Priya"]);
    let meta = std::fs::read_to_string(f.dir().join("session.json")).unwrap();
    assert!(!meta.contains(pipeline::UNLABELLED_WARNING), "{meta}");
    assert!(!f.dir().join(CARRY_FILE).exists());
}

#[test]
fn missing_models_are_caught_before_any_stage() {
    let f = Fixture::new("no-models");
    let e = f.engine.check().unwrap_err();
    assert!(format!("{e:#}").contains("fetch-models"), "{e:#}");
}
