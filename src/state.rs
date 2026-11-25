//! App phases and their only legal transitions. Each transition consumes the
//! phase, so a `Live` recording always moves into the next phase or back into
//! the old one; it cannot be left behind.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Instant;

use anyhow::anyhow;
use jiff::Zoned;

use crate::session::{self, SessionDir, StopSignalled};

/// Keyed by bundle id so declining one call never touches another. A decline
/// lasts only while that app keeps producing audio.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Declined(BTreeSet<String>);

impl Declined {
    pub fn decline(&mut self, app: &str) {
        self.0.insert(app.to_string());
    }

    pub fn is_declined(&self, app: &str) -> bool {
        self.0.contains(app)
    }

    pub fn retire(&mut self, live: &[String]) {
        self.0.retain(|a| live.iter().any(|l| l == a));
    }
}

/// `dir` is claimed on the main thread before the worker spawns, so Stop's
/// sentinel lands where the capture writes even if the sessions folder has
/// since changed. An `Arc` only because the worker shares it; `SessionDir` is
/// deliberately not `Clone`.
pub struct Live {
    pub dir: Arc<SessionDir>,
    pub started: Instant,
    /// Set when the watcher started this recording for a call.
    pub app: Option<String>,
    /// Owned by whichever phase holds the `Live`; a deferred quit is answered
    /// through it.
    pub result: Receiver<anyhow::Result<PathBuf>>,
    pub meter: Arc<session::Meter>,
}

pub enum Phase {
    Idle {
        declined: Declined,
    },
    /// Waiting for consent to record a watched app.
    Armed {
        app: String,
        declined: Declined,
    },
    Recording {
        live: Live,
        declined: Declined,
    },
    /// Transcription is not a phase: it belongs to [`crate::queue::Queue`], so
    /// the app is `Idle` and free to record while it runs.
    Stopping {
        live: Live,
        stopped: StopSignalled,
        declined: Declined,
    },
    /// A failed stop is not `Failed`: it stays `Recording`, because the worker
    /// is still recording.
    Failed {
        dir: Option<PathBuf>,
        error: String,
        at: Zoned,
        declined: Declined,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseKind {
    Idle,
    Armed,
    Recording,
    Stopping,
    Failed,
}

impl PhaseKind {
    /// Every variant, so a check walks them instead of keeping its own list.
    pub const ALL: [PhaseKind; 5] = [
        PhaseKind::Idle,
        PhaseKind::Armed,
        PhaseKind::Recording,
        PhaseKind::Stopping,
        PhaseKind::Failed,
    ];

    pub fn symbol(self) -> &'static str {
        match self {
            PhaseKind::Idle => "waveform",
            PhaseKind::Armed => "waveform.badge.exclamationmark",
            PhaseKind::Recording => "waveform.circle.fill",
            PhaseKind::Stopping => "hourglass",
            PhaseKind::Failed => "exclamationmark.triangle",
        }
    }

    /// `Failed` must be watched, or the app would be deaf to every later call.
    pub fn is_watching(self) -> bool {
        matches!(self, PhaseKind::Idle | PhaseKind::Armed | PhaseKind::Failed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhaseView {
    pub kind: PhaseKind,
    pub has_worker: bool,
}

/// Proof that something is alive to answer a deferred quit. The field is
/// private and [`defer`], the only producer, refuses a phase with no worker.
#[derive(Debug)]
pub struct Deferred(());

/// What `applicationShouldTerminate:` should answer.
#[derive(Debug)]
pub enum Quit {
    Now,
    Later(Deferred),
    /// A recording whose stop failed is still recording; killing it would
    /// abandon the conversation.
    Cancel(anyhow::Error),
}

fn defer(phase: Phase, queue_busy: bool) -> (Phase, Quit) {
    if phase.live().is_some() || queue_busy {
        let q = Quit::Later(Deferred(()));
        (phase, q)
    } else {
        // Unreachable by construction; deferring with nothing to reply hangs.
        (phase, Quit::Now)
    }
}

impl Phase {
    pub fn idle() -> Phase {
        Phase::Idle {
            declined: Declined::default(),
        }
    }

    pub fn kind(&self) -> PhaseKind {
        match self {
            Phase::Idle { .. } => PhaseKind::Idle,
            Phase::Armed { .. } => PhaseKind::Armed,
            Phase::Recording { .. } => PhaseKind::Recording,
            Phase::Stopping { .. } => PhaseKind::Stopping,
            Phase::Failed { .. } => PhaseKind::Failed,
        }
    }

    pub fn view(&self) -> PhaseView {
        PhaseView {
            kind: self.kind(),
            has_worker: self.live().is_some(),
        }
    }

    pub fn live(&self) -> Option<&Live> {
        match self {
            Phase::Recording { live, .. } | Phase::Stopping { live, .. } => Some(live),
            _ => None,
        }
    }

    pub fn declined(&self) -> &Declined {
        match self {
            Phase::Idle { declined }
            | Phase::Armed { declined, .. }
            | Phase::Recording { declined, .. }
            | Phase::Stopping { declined, .. }
            | Phase::Failed { declined, .. } => declined,
        }
    }

    pub fn declined_mut(&mut self) -> &mut Declined {
        match self {
            Phase::Idle { declined }
            | Phase::Armed { declined, .. }
            | Phase::Recording { declined, .. }
            | Phase::Stopping { declined, .. }
            | Phase::Failed { declined, .. } => declined,
        }
    }

    pub fn app(&self) -> Option<String> {
        match self {
            Phase::Armed { app, .. } => Some(app.clone()),
            Phase::Recording { live, .. } | Phase::Stopping { live, .. } => live.app.clone(),
            _ => None,
        }
    }

    pub fn failure(&self) -> Option<&str> {
        match self {
            Phase::Failed { error, .. } => Some(error),
            _ => None,
        }
    }

    pub fn failed_dir(&self) -> Option<&Path> {
        match self {
            Phase::Failed { dir, .. } => dir.as_deref(),
            _ => None,
        }
    }

    /// On failure the phase stays `Recording`: the worker is still recording,
    /// so Stop must stay enabled.
    pub fn stop(self) -> Result<Phase, (Phase, anyhow::Error)> {
        match self {
            Phase::Recording { live, declined } => match session::signal_stop(&live.dir) {
                Ok(stopped) => Ok(Phase::Stopping {
                    live,
                    stopped,
                    declined,
                }),
                Err(e) => Err((Phase::Recording { live, declined }, e)),
            },
            other => Err((other, anyhow!("not recording"))),
        }
    }

    pub fn finished(self, r: anyhow::Result<PathBuf>) -> Phase {
        let (dir, declined) = match self {
            Phase::Recording { live, declined } | Phase::Stopping { live, declined, .. } => {
                (Some(live.dir.path().to_path_buf()), declined)
            }
            // A result in a phase with no worker is a caller bug, and no reason
            // to throw the declines away.
            other => return other,
        };
        match r {
            Ok(_) => Phase::Idle { declined },
            Err(e) => Phase::Failed {
                dir,
                error: format!("{e:#}"),
                at: crate::clock::now(),
                declined,
            },
        }
    }

    /// Only `Idle` becomes `Failed`: a recording or a pending consent question
    /// must not be interrupted by old news, and the banner still carries it.
    pub fn transcript_failed(self, dir: PathBuf, error: &anyhow::Error) -> Phase {
        match self {
            Phase::Idle { declined } => Phase::Failed {
                dir: Some(dir),
                error: format!("{error:#}"),
                at: crate::clock::now(),
                declined,
            },
            other => other,
        }
    }

    pub fn dismiss(self) -> Phase {
        match self {
            Phase::Failed { declined, .. } => Phase::Idle { declined },
            other => other,
        }
    }

    pub fn arm(self, app: String) -> Phase {
        match self.dismiss() {
            Phase::Idle { declined } => Phase::Armed { app, declined },
            other => other,
        }
    }

    pub fn disarm(self) -> Phase {
        match self {
            Phase::Armed { declined, .. } => Phase::Idle { declined },
            other => other,
        }
    }

    /// A phase that already owns a worker hands the `Live` back: dropping its
    /// receiver would orphan a running capture.
    pub fn start(self, live: Live) -> (Phase, Option<Live>) {
        match self.dismiss() {
            Phase::Idle { declined } | Phase::Armed { declined, .. } => {
                (Phase::Recording { live, declined }, None)
            }
            other => (other, Some(live)),
        }
    }

    /// Stopping or waving a call away records a decline; otherwise the watcher
    /// sees the app still playing and starts a fresh recording.
    pub fn decline_this_call(mut self, watched: &[String], playing: &[String]) -> Phase {
        let app = watched
            .iter()
            .find(|a| playing.iter().any(|l| l == *a))
            .cloned()
            .or_else(|| self.app());
        if let Some(app) = app {
            self.declined_mut().decline(&app);
        }
        self
    }

    /// | Phase | Reply |
    /// |---|---|
    /// | `Idle`, `Armed`, `Failed`, queue idle | `NSTerminateNow` |
    /// | `Idle`, `Armed`, `Failed`, queue busy | `NSTerminateLater` — the queue's worker answers |
    /// | `Recording`, stop succeeds | → `Stopping`, `NSTerminateLater` |
    /// | `Recording`, stop fails | `NSTerminateCancel` — the capture keeps running |
    /// | `Stopping` | `NSTerminateLater` |
    pub fn on_quit(self, queue_busy: bool) -> (Phase, Quit) {
        match self {
            Phase::Idle { .. } | Phase::Armed { .. } | Phase::Failed { .. } => {
                defer(self, queue_busy)
            }
            Phase::Stopping { .. } => defer(self, queue_busy),
            Phase::Recording { .. } => match self.stop() {
                Ok(next) => defer(next, queue_busy),
                Err((back, e)) => (back, Quit::Cancel(e)),
            },
        }
    }
}

const REENTRANT: &str = "ambient: the phase was moved while a transition was already running — \
                         a handler called back into the state from inside a transition closure";

/// The borrow is held for the whole of a transition, so an AppKit callback
/// firing mid-move panics with [`REENTRANT`] instead of seeing `None`.
pub struct PhaseCell(RefCell<Option<Phase>>);

impl PhaseCell {
    pub fn new(phase: Phase) -> Self {
        PhaseCell(RefCell::new(Some(phase)))
    }

    /// `f` cannot touch this cell; run effects at the call site, after the
    /// borrow drops.
    pub fn transition<T>(&self, f: impl FnOnce(Phase) -> (Phase, T)) -> T {
        let mut slot = self
            .0
            .try_borrow_mut()
            .unwrap_or_else(|_| panic!("{REENTRANT}"));
        let phase = slot.take().unwrap_or_else(|| panic!("{REENTRANT}"));
        let (next, out) = f(phase);
        *slot = Some(next);
        out
    }

    pub fn snapshot(&self) -> PhaseView {
        self.with(Phase::view)
    }

    pub fn with<T>(&self, f: impl FnOnce(&Phase) -> T) -> T {
        let slot = self
            .0
            .try_borrow()
            .unwrap_or_else(|_| panic!("{REENTRANT}"));
        f(slot.as_ref().unwrap_or_else(|| panic!("{REENTRANT}")))
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::mpsc::{channel, Sender};

    pub(crate) fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "ambient-state-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&p).ok();
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Returns the `Sender` so the channel stays connected while the test
    /// needs it.
    pub(crate) fn recording(
        root: &std::path::Path,
        app: Option<&str>,
        declined: Declined,
    ) -> (Phase, Sender<anyhow::Result<PathBuf>>) {
        let (tx, rx) = channel();
        let live = Live {
            dir: Arc::new(SessionDir::claim(root, None).unwrap()),
            started: Instant::now(),
            app: app.map(str::to_string),
            result: rx,
            meter: Arc::new(session::Meter::default()),
        };
        (Phase::Recording { live, declined }, tx)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn a_failed_stop_hands_the_recording_back() {
        let root = scratch("failedstop");
        let (phase, _tx) = recording(&root, Some("us.zoom.xos"), Declined::default());
        let dir = phase.live().unwrap().dir.path().to_path_buf();
        // Leaves the stop sentinel nowhere to land.
        std::fs::remove_dir_all(&dir).unwrap();

        let Err((back, e)) = phase.stop() else {
            panic!("stop reported success with no directory to write the sentinel into");
        };
        assert_eq!(back.kind(), PhaseKind::Recording);
        assert!(
            back.live().is_some(),
            "the worker is still recording, so the receiver must survive the failure"
        );
        assert!(
            e.to_string().contains("STOP"),
            "the error should name what could not be written, got: {e}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_successful_stop_writes_the_sentinel_and_moves_to_transcribing() {
        let root = scratch("goodstop");
        let (phase, _tx) = recording(&root, None, Declined::default());
        let dir = phase.live().unwrap().dir.path().to_path_buf();

        let next = phase
            .stop()
            .unwrap_or_else(|(_, e)| panic!("stop failed: {e}"));
        assert_eq!(next.kind(), PhaseKind::Stopping);
        assert!(dir.join(session::STOP_FILE).is_file());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_quit_is_only_deferred_when_a_worker_can_answer_it() {
        let root = scratch("quit");

        for phase in [
            Phase::idle(),
            Phase::idle().arm("us.zoom.xos".into()),
            Phase::Failed {
                dir: None,
                error: "the models did not load".into(),
                at: crate::clock::now(),
                declined: Declined::default(),
            },
        ] {
            let (after, q) = phase.on_quit(false);
            assert!(
                matches!(q, Quit::Now),
                "{:?} has no worker, so nothing can be deferred for",
                after.kind()
            );
        }

        let (phase, _tx) = recording(&root, None, Declined::default());
        let (after, q) = phase.on_quit(false);
        assert!(matches!(q, Quit::Later(_)));
        assert_eq!(after.kind(), PhaseKind::Stopping);
        assert!(after.live().is_some());

        let (after, q) = after.on_quit(false);
        assert!(matches!(q, Quit::Later(_)));
        assert!(after.live().is_some());

        let (phase, _tx) = recording(&root, None, Declined::default());
        std::fs::remove_dir_all(phase.live().unwrap().dir.path()).unwrap();
        let (after, q) = phase.on_quit(false);
        assert!(
            matches!(q, Quit::Cancel(_)),
            "abandoning a running capture is the failure the deferred quit exists to prevent"
        );
        assert_eq!(after.kind(), PhaseKind::Recording);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn only_a_worker_error_enters_failed_and_it_is_leavable() {
        let root = scratch("failed");

        let (phase, _tx) = recording(&root, None, Declined::default());
        let ok = phase.finished(Ok(PathBuf::from("/tmp/whatever")));
        assert_eq!(ok.kind(), PhaseKind::Idle);

        let (phase, _tx) = recording(&root, None, Declined::default());
        let dir = phase.live().unwrap().dir.path().to_path_buf();
        let failed = phase.finished(Err(anyhow!("the models did not load")));
        assert_eq!(failed.kind(), PhaseKind::Failed);
        assert_eq!(failed.failure(), Some("the models did not load"));
        assert!(matches!(&failed, Phase::Failed { dir: Some(d), .. } if *d == dir));

        assert_eq!(failed.dismiss().kind(), PhaseKind::Idle);

        let (phase, _tx) = recording(&root, None, Declined::default());
        let failed = phase.finished(Err(anyhow!("boom")));
        assert!(
            failed.kind().is_watching(),
            "Failed must not make the app deaf"
        );
        assert_eq!(failed.arm("us.zoom.xos".into()).kind(), PhaseKind::Armed);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn declines_survive_every_transition() {
        let root = scratch("declines");
        let mut d = Declined::default();
        d.decline("us.zoom.xos");

        let (phase, _tx) = recording(&root, None, d);
        let phase = phase.stop().unwrap_or_else(|(_, e)| panic!("{e}"));
        assert!(phase.declined().is_declined("us.zoom.xos"));
        let phase = phase.finished(Err(anyhow!("boom")));
        assert!(phase.declined().is_declined("us.zoom.xos"));
        let phase = phase.dismiss().arm("com.microsoft.teams2".into());
        assert!(phase.declined().is_declined("us.zoom.xos"));
        assert!(phase.disarm().declined().is_declined("us.zoom.xos"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn stopping_declines_the_call_it_was_recording() {
        let root = scratch("declinecall");
        let watched = vec!["us.zoom.xos".to_string()];

        let (phase, _tx) = recording(&root, Some("us.zoom.xos"), Declined::default());
        let phase = phase.decline_this_call(&watched, &[]);
        assert!(phase.declined().is_declined("us.zoom.xos"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[should_panic(expected = "transition")]
    fn a_re_entrant_transition_panics_rather_than_corrupting_state() {
        let cell = PhaseCell::new(Phase::idle());
        cell.transition(|p| {
            // Exactly the shape of an AppKit callback firing mid-move.
            let _ = cell.snapshot();
            (p, ())
        });
    }

    #[test]
    fn a_transition_puts_the_phase_back() {
        let cell = PhaseCell::new(Phase::idle());
        let armed = cell.transition(|p| {
            let p = p.arm("us.zoom.xos".into());
            let k = p.kind();
            (p, k)
        });
        assert_eq!(armed, PhaseKind::Armed);
        assert_eq!(cell.snapshot().kind, PhaseKind::Armed);
        assert!(!cell.snapshot().has_worker);
    }

    #[test]
    fn a_busy_queue_defers_a_quit_from_every_workerless_phase() {
        let root = scratch("quitqueue");
        for phase in [
            Phase::idle(),
            Phase::idle().arm("us.zoom.xos".into()),
            Phase::Failed {
                dir: None,
                error: "boom".into(),
                at: crate::clock::now(),
                declined: Declined::default(),
            },
        ] {
            let kind = phase.kind();
            let (after, q) = phase.on_quit(true);
            assert!(
                matches!(q, Quit::Later(_)),
                "{kind:?} with a busy queue must wait for it"
            );
            assert_eq!(after.kind(), kind, "the phase itself does not move");
        }
        let (phase, _tx) = recording(&root, None, Declined::default());
        let (after, q) = phase.on_quit(true);
        assert!(matches!(q, Quit::Later(_)));
        assert_eq!(after.kind(), PhaseKind::Stopping);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_failed_transcript_enters_failed_only_from_idle() {
        let root = scratch("transcriptfailed");
        let e = anyhow!("the models did not load");
        let dir = PathBuf::from("/s/2026-09-02T0900");

        let failed = Phase::idle().transcript_failed(dir.clone(), &e);
        assert_eq!(failed.kind(), PhaseKind::Failed);
        assert!(matches!(&failed, Phase::Failed { dir: Some(d), .. } if *d == dir));
        assert_eq!(failed.failure(), Some("the models did not load"));

        let armed = Phase::idle().arm("us.zoom.xos".into());
        assert_eq!(
            armed.transcript_failed(dir.clone(), &e).kind(),
            PhaseKind::Armed,
            "a call being asked about is not interrupted by old news"
        );
        let (rec, _tx) = recording(&root, None, Declined::default());
        assert_eq!(
            rec.transcript_failed(dir, &e).kind(),
            PhaseKind::Recording,
            "a recording in progress is not interrupted either"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
