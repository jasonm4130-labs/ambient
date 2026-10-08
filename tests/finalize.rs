//! Finalizing in a child process, against the real `ambient` binary. No model
//! is ever loaded: a session whose live pass is complete needs none, and the
//! model files here are empty stand-ins that fail to load.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ambient::finalize;
use ambient::session::{self, Meter, MeterPhase, STATUS_FILE, TRANSCRIBING_LOCK};

const ID: &str = "2026-10-08T09-00-00";

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(test: &str) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("ambient-finalize-{test}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let dir = root.join("sessions").join(ID);
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        let models = root.join("models");
        std::fs::create_dir_all(models.join("sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8")).unwrap();
        std::fs::write(models.join("silero_vad.onnx"), "").unwrap();
        std::fs::write(root.join("config.json"), r#"{"diarize": false}"#).unwrap();
        std::fs::write(
            dir.join("session.json"),
            format!(
                r#"{{"id":"{ID}","name":null,"started_at":"2026-10-08T09:00:00+00:00",
                "ended_at":"2026-10-08T09:01:00+00:00","duration_s":60.0,"device_hz":16000,
                "channels":1,"mic_channels":1,"apps":[],"model":"stand-in"}}"#
            ),
        )
        .unwrap();
        let mut wav = hound::WavWriter::create(
            dir.join("audio").join("room.wav"),
            hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        wav.write_sample(0i16).unwrap();
        wav.finalize().unwrap();
        std::fs::write(dir.join(STATUS_FILE), "captured\n").unwrap();
        Fixture { root }
    }

    fn dir(&self) -> PathBuf {
        self.root.join("sessions").join(ID)
    }

    fn lane(&self) -> PathBuf {
        self.root.join("decoder.lock")
    }

    /// A live pass that finished during the recording: nothing left to
    /// decode, so finalizing writes the markdown without loading a model.
    fn transcribed_live(&self) -> &Fixture {
        let raw = concat!(
            r#"{"track":"room","start_ms":1000,"end_ms":2000,"text":"the budget is approved","confidence":0.9}"#,
            "\n",
            r#"{"track":"room","start_ms":3000,"end_ms":4000,"text":"ship it on friday","confidence":0.9}"#,
            "\n"
        );
        std::fs::write(self.dir().join("raw.jsonl"), raw).unwrap();
        std::fs::write(
            self.dir().join("live-asr.json"),
            format!(
                r#"{{"version":1,"rates":[16000,16000],"tracks":[{{"read":16000,"carry":null}},
                {{"read":16000,"carry":null}}],"raw_len":{},"complete":true,"pending":null}}"#,
                raw.len()
            ),
        )
        .unwrap();
        self
    }

    fn command(&self) -> Command {
        let mut cmd = finalize::command(Path::new(env!("CARGO_BIN_EXE_ambient")), &self.dir());
        cmd.env("AMBIENT_HOME", self.root.join("sessions"))
            .env("AMBIENT_MODELS", self.root.join("models"))
            .env("AMBIENT_CONFIG", self.root.join("config.json"))
            .env("AMBIENT_ROSTER", self.root.join("roster.json"))
            .env(ambient::live::DECODER_ENV, self.lane());
        cmd
    }

    fn status(&self) -> String {
        std::fs::read_to_string(self.dir().join(STATUS_FILE)).unwrap_or_default()
    }

    fn lock(&self) -> Option<u32> {
        std::fs::read_to_string(self.dir().join(TRANSCRIBING_LOCK))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// What the app checks at launch to re-queue a session a crash left.
    fn requeued(&self) -> bool {
        session::captured_awaiting_transcript(&self.root.join("sessions")).contains(&self.dir())
    }

    /// Take the decoder lane as another process's recogniser would, so the
    /// child blocks before its first model load with its lock already taken.
    fn hold_lane(&self) -> File {
        let file = File::create(self.lane()).unwrap();
        assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) }, 0);
        file
    }

    /// The child's pid, once it has claimed the session.
    fn wait_for_child_lock(&self) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(pid) = self.lock() {
                if pid != std::process::id() {
                    return pid;
                }
            }
            assert!(
                Instant::now() < deadline,
                "the child never claimed the session"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

#[test]
fn a_child_finalizes_the_session_and_reports_its_progress() {
    let fx = Fixture::new("ok");
    fx.transcribed_live();
    let meter = Arc::new(Meter::default());
    let out = finalize::run(fx.command(), &fx.dir(), &meter).expect("finalize succeeds");
    assert_eq!(out, fx.dir());
    let md = std::fs::read_to_string(fx.dir().join("transcript.md")).unwrap();
    assert!(md.contains("the budget is approved"), "{md}");
    assert_eq!(fx.status(), "done\n");
    assert_eq!(meter.phase(), MeterPhase::Done);
    assert_eq!(fx.lock(), None, "the child released the session");
    assert!(!fx.requeued());
}

#[test]
fn a_childs_own_failure_is_surfaced_and_leaves_the_session_requeueable() {
    let fx = Fixture::new("fail");
    // No live pass, so the child loads the VAD model — an empty file.
    let meter = Arc::new(Meter::default());
    let err = finalize::run(fx.command(), &fx.dir(), &meter).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("silero_vad.onnx"),
        "the child's own reason: {msg}"
    );
    assert_eq!(fx.status(), format!("failed: {msg}\n"));
    assert_eq!(meter.phase(), MeterPhase::Failed);
    assert_eq!(fx.lock(), None);
    assert!(
        fx.requeued(),
        "a failed session is retried at the next launch"
    );
}

#[test]
fn a_child_that_crashes_leaves_the_session_as_an_in_process_failure_would() {
    let fx = Fixture::new("crash");
    let lane = fx.hold_lane();
    let meter = Arc::new(Meter::default());
    let job = {
        let (cmd, dir, meter) = (fx.command(), fx.dir(), meter.clone());
        std::thread::spawn(move || finalize::run(cmd, &dir, &meter))
    };
    let pid = fx.wait_for_child_lock();
    // Mid-job: past the claim and the status write, blocked on the lane.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !fx.status().starts_with("transcribing") {
        assert!(Instant::now() < deadline, "{}", fx.status());
        std::thread::sleep(Duration::from_millis(20));
    }
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    let err = job.join().unwrap().unwrap_err();
    drop(lane);
    assert!(format!("{err:#}").contains("signal 9"), "{err:#}");
    assert!(fx.status().starts_with("failed: "), "{}", fx.status());
    assert_eq!(meter.phase(), MeterPhase::Failed);
    assert_eq!(fx.lock(), None, "the dead child's lock is cleared");
    assert!(fx.requeued());
}

#[test]
fn a_child_stops_when_its_parent_goes_away() {
    let fx = Fixture::new("orphan");
    let lane = fx.hold_lane();
    let mut child = fx.command().spawn().unwrap();
    let pid = fx.wait_for_child_lock();
    assert_eq!(pid, child.id());
    // What the kernel does to the pipe when the parent dies.
    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the child outlived its parent");
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(lane);
    assert!(!status.success());
    // Nobody is left to tidy up: the lock names a dead pid, which is stale,
    // and the session goes back on the queue at the next launch.
    assert!(fx.requeued());
}
