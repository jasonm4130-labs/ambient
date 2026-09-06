//! What the app is doing, and the only legal ways to move between those states.
//!
//! Every transition consumes the phase and hands back the next one, so a
//! recording cannot be left behind by a state change: either the `Live` moves
//! into the new phase or it moves back into the old one. The two witness types
//! from [`crate::session`] do the rest — a `Stopping` phase cannot be
//! built without a [`StopSignalled`], and a `StopSignalled` cannot be built
//! without having written the sentinel into a directory this process claimed.
//!
//! Nothing here touches AppKit. That is the point: [`PhaseCell::transition`]
//! runs its closure with the cell borrowed, so a handler cannot re-enter the
//! state while it is mid-move, and every effect runs after the borrow drops.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Instant;

use anyhow::anyhow;
use chrono::{DateTime, Local};

use crate::session::{self, SessionDir, StopSignalled};

/// Bundles the user has said "not this one" to, keyed by bundle id so
/// declining one call can never touch the answer about another. A decline
/// lasts only as long as that app keeps producing audio — [`Declined::retire`]
/// is what lets the next call from the same app be asked about afresh.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Declined(BTreeSet<String>);

impl Declined {
    pub fn decline(&mut self, app: &str) {
        self.0.insert(app.to_string());
    }

    pub fn is_declined(&self, app: &str) -> bool {
        self.0.contains(app)
    }

    /// Drop every declined app that has stopped producing audio. Called
    /// unconditionally at the top of every poll, which is the whole
    /// replacement for a separate "forget" outcome.
    pub fn retire(&mut self, live: &[String]) {
        self.0.retain(|a| live.iter().any(|l| l == a));
    }
}

/// A recording this process started and still owns.
///
/// `dir` is claimed on the main thread *before* the worker spawns, which is
/// what makes stopping honest: the sentinel goes into the directory this
/// recording is actually writing to, rather than into whatever `home()` says
/// at the moment Stop is pressed. Changing the sessions folder mid-recording
/// used to make the running capture unreachable.
///
/// It is an `Arc` only because the worker needs the same directory to record
/// into; `SessionDir` is deliberately not `Clone`.
pub struct Live {
    pub dir: Arc<SessionDir>,
    pub started: Instant,
    /// Which call this recording belongs to, when it was started by the
    /// watcher rather than by the Start item.
    pub app: Option<String>,
    /// Exactly one owner, and it is whichever phase currently holds the
    /// `Live`. A deferred quit is answerable only because this exists.
    pub result: Receiver<anyhow::Result<PathBuf>>,
    /// Shared with the worker thread, which writes it once a second. Reading
    /// this rather than the session's status file is what let the
    /// `read_dir`/sort/pop status fallback be deleted outright instead of
    /// repaired.
    pub meter: Arc<session::Meter>,
}

/// Which state the app is in. The variant governs which transitions are
/// legal; what the user is told about a running capture comes from the
/// worker's `Meter`, shared through `Live`.
pub enum Phase {
    Idle {
        declined: Declined,
    },
    /// A watched app started producing audio and Ambient is waiting to be told
    /// whether to record it.
    Armed {
        app: String,
        declined: Declined,
    },
    Recording {
        live: Live,
        declined: Declined,
    },
    /// Stop has been signalled and the capture worker is still finalising
    /// the audio — seconds, not minutes. Transcription is not a phase at
    /// all: it belongs to [`crate::queue::Queue`], so the app is `Idle` and
    /// free to record again while it runs. Reachable only through
    /// [`Phase::stop`], because `StopSignalled` has no other producer.
    Stopping {
        live: Live,
        stopped: StopSignalled,
        declined: Declined,
    },
    /// A worker came back with an error: the capture's, through
    /// [`Phase::finished`], or the transcription queue's, through
    /// [`Phase::transcript_failed`]. A failed *stop* stays `Recording`,
    /// because the worker really is still recording. Left by
    /// [`Phase::dismiss`], or implicitly by arming or starting.
    Failed {
        dir: Option<PathBuf>,
        error: String,
        at: DateTime<Local>,
        declined: Declined,
    },
}

/// A phase with its payload stripped off: cheap, `Copy`, and enough to decide
/// what the menu and (from step 5) the window should draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseKind {
    Idle,
    Armed,
    Recording,
    Stopping,
    Failed,
}

impl PhaseKind {
    /// Every variant, so a check can walk them instead of repeating them.
    /// `symbolcheck` kept its own hand-written list and validated four symbols
    /// for five states the moment `Failed` was added — which is precisely the
    /// silent wrong-icon failure that check exists to catch.
    pub const ALL: [PhaseKind; 5] = [
        PhaseKind::Idle,
        PhaseKind::Armed,
        PhaseKind::Recording,
        PhaseKind::Stopping,
        PhaseKind::Failed,
    ];

    /// SF Symbols standing in for the artboard's icons: outline when present
    /// but recording nothing, filled while live, a distinct shape while the
    /// models are running, and a warning when the last attempt failed.
    pub fn symbol(self) -> &'static str {
        match self {
            PhaseKind::Idle => "waveform",
            PhaseKind::Armed => "waveform.badge.exclamationmark",
            PhaseKind::Recording => "waveform.circle.fill",
            PhaseKind::Stopping => "hourglass",
            PhaseKind::Failed => "exclamationmark.triangle",
        }
    }

    /// `Failed` is watched exactly like `Idle`. It has to be: polling used to
    /// be gated on Idle-or-Armed, so a phase with no way out of it would make
    /// the app deaf to every later call.
    pub fn is_watching(self) -> bool {
        matches!(self, PhaseKind::Idle | PhaseKind::Armed | PhaseKind::Failed)
    }
}

/// The `Copy` summary [`PhaseCell::snapshot`] returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhaseView {
    pub kind: PhaseKind,
    /// Whether a worker is alive to answer for itself. The Stop item and the
    /// deferred-quit reply turn on this and nothing else.
    pub has_worker: bool,
}

/// Proof that something is alive to answer a deferred quit.
///
/// The field is private to this module, so `Quit::Later` cannot be built
/// anywhere else — and inside this module the only producer is [`defer`],
/// which refuses any phase without a `Live`. That is the pairing bug 1
/// required: a quit deferred with nothing running to reply to it.
#[derive(Debug)]
pub struct Deferred(());

/// What `applicationShouldTerminate:` should answer.
#[derive(Debug)]
pub enum Quit {
    Now,
    Later(Deferred),
    /// Refuse the quit and say why. A live recording whose stop failed is
    /// still recording; killing it here would abandon the conversation, which
    /// is exactly what deferring the quit was written to prevent.
    Cancel(anyhow::Error),
}

/// The only producer of [`Quit::Later`]. Two things can answer a deferred
/// quit: the capture worker this phase holds, or the transcription queue's
/// worker when `queue_busy` says it has jobs.
fn defer(phase: Phase, queue_busy: bool) -> (Phase, Quit) {
    if phase.live().is_some() || queue_busy {
        let q = Quit::Later(Deferred(()));
        (phase, q)
    } else {
        // Unreachable by construction, and the safe answer if it ever is not:
        // deferring with nothing to reply with is the hang this change exists
        // to remove.
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

    /// The app this phase is about, when it is about one.
    pub fn app(&self) -> Option<String> {
        match self {
            Phase::Armed { app, .. } => Some(app.clone()),
            Phase::Recording { live, .. } | Phase::Stopping { live, .. } => live.app.clone(),
            _ => None,
        }
    }

    /// The banner text a failure left behind, if the app is showing one.
    pub fn failure(&self) -> Option<&str> {
        match self {
            Phase::Failed { error, .. } => Some(error),
            _ => None,
        }
    }

    /// The directory a failure was about, when it had one.
    pub fn failed_dir(&self) -> Option<&Path> {
        match self {
            Phase::Failed { dir, .. } => dir.as_deref(),
            _ => None,
        }
    }

    /// Ask the running capture to stop.
    ///
    /// On failure the recording is handed back and the phase stays
    /// `Recording`, which is the honest state: the worker really is still
    /// recording, so Stop must stay enabled and Start must stay disabled.
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

    /// The worker handed back a result. Success returns to `Idle`; failure is
    /// the one and only entry to `Failed`, so a recording that produced audio
    /// and no transcript stops being drawn as "nothing happened".
    pub fn finished(self, r: anyhow::Result<PathBuf>) -> Phase {
        let (dir, declined) = match self {
            Phase::Recording { live, declined } | Phase::Stopping { live, declined, .. } => {
                (Some(live.dir.path().to_path_buf()), declined)
            }
            // A result arriving in a phase that owns no worker is a caller
            // bug, and no reason to throw the declines away.
            other => return other,
        };
        match r {
            Ok(_) => Phase::Idle { declined },
            Err(e) => Phase::Failed {
                dir,
                error: format!("{e:#}"),
                at: Local::now(),
                declined,
            },
        }
    }

    /// The transcription queue failed a session. `Idle` becomes `Failed` so
    /// the failure is drawn where a capture failure would be; any other phase
    /// is left alone — a recording in progress or a call being asked about
    /// must not be interrupted by old news, and the delegate's banner still
    /// carries the message.
    pub fn transcript_failed(self, dir: PathBuf, error: &anyhow::Error) -> Phase {
        match self {
            Phase::Idle { declined } => Phase::Failed {
                dir: Some(dir),
                error: format!("{error:#}"),
                at: Local::now(),
                declined,
            },
            other => other,
        }
    }

    /// Clear a failure banner. Keeps the declines, like every other move.
    pub fn dismiss(self) -> Phase {
        match self {
            Phase::Failed { declined, .. } => Phase::Idle { declined },
            other => other,
        }
    }

    /// Wait for consent about `app`. Dismisses a stale failure implicitly.
    pub fn arm(self, app: String) -> Phase {
        match self.dismiss() {
            Phase::Idle { declined } => Phase::Armed { app, declined },
            other => other,
        }
    }

    /// Stand down from `Armed` without recording.
    pub fn disarm(self) -> Phase {
        match self {
            Phase::Armed { declined, .. } => Phase::Idle { declined },
            other => other,
        }
    }

    /// Take ownership of a freshly spawned worker. Dismisses a stale failure
    /// implicitly. A phase that already owns one refuses, handing the `Live`
    /// straight back rather than dropping it: dropping the receiver would
    /// orphan a capture that is still running.
    pub fn start(self, live: Live) -> (Phase, Option<Live>) {
        match self.dismiss() {
            Phase::Idle { declined } | Phase::Armed { declined, .. } => {
                (Phase::Recording { live, declined }, None)
            }
            other => (other, Some(live)),
        }
    }

    /// Stopping, or waving a call away, is an answer about that call.
    ///
    /// Without recording it, the watcher sees the app still playing on its
    /// next tick and starts a fresh recording — leaving no way to stop until
    /// the call ends.
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

    /// The quit table.
    ///
    /// | Phase | Reply |
    /// |---|---|
    /// | `Idle`, `Armed`, `Failed`, queue idle | `NSTerminateNow` — nothing exists to defer for |
    /// | `Idle`, `Armed`, `Failed`, queue busy | `NSTerminateLater` — the queue's worker answers |
    /// | `Recording`, stop succeeds | → `Stopping`, `NSTerminateLater` |
    /// | `Recording`, stop fails | `NSTerminateCancel` — the capture keeps running |
    /// | `Stopping` | `NSTerminateLater` — it owns the receiver by construction |
    ///
    /// `queue_busy` is whether the transcription queue still holds a job.
    /// Quitting mid-queue finishes the outstanding work rather than
    /// abandoning it, exactly as quitting mid-recording does.
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

/// The one home of the app's state.
///
/// A consuming transition has to move the phase out of the cell, and the
/// window this opens — an AppKit callback firing while the value is out — is
/// closed structurally rather than by a comment: the borrow is held for the
/// whole of the closure, so the closure cannot reach the state, and a
/// re-entrant call panics with [`REENTRANT`] instead of quietly seeing `None`.
pub struct PhaseCell(RefCell<Option<Phase>>);

impl PhaseCell {
    pub fn new(phase: Phase) -> Self {
        PhaseCell(RefCell::new(Some(phase)))
    }

    /// Move the phase through `f` and return whatever `f` says the caller
    /// should do about it. `f` must be pure: it cannot touch AppKit usefully,
    /// and it cannot touch this cell at all. Effects run at the call site,
    /// after the borrow has dropped.
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

    /// A cheap `Copy` summary, for deciding what to draw.
    pub fn snapshot(&self) -> PhaseView {
        self.with(Phase::view)
    }

    /// Read the phase without moving it.
    pub fn with<T>(&self, f: impl FnOnce(&Phase) -> T) -> T {
        let slot = self
            .0
            .try_borrow()
            .unwrap_or_else(|_| panic!("{REENTRANT}"));
        f(slot.as_ref().unwrap_or_else(|| panic!("{REENTRANT}")))
    }
}

/// Builders the watcher's and the delegate's tests need. A `Live` is only
/// honestly constructible by claiming a directory and spawning a worker, so
/// tests do the first half and stand in for the second.
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

    /// A `Recording` over a real claimed directory. The `Sender` is returned
    /// so the channel stays connected for as long as the test wants it.
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

    /// The heart of bug 1. A stop that fails must not advance the state: the
    /// worker is still recording, so the phase that says so is `Recording`.
    #[test]
    fn a_failed_stop_hands_the_recording_back() {
        let root = scratch("failedstop");
        let (phase, _tx) = recording(&root, Some("us.zoom.xos"), Declined::default());
        let dir = phase.live().unwrap().dir.path().to_path_buf();
        // The sentinel has nowhere to land. This is a mid-recording sessions
        // folder move, one layer down.
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

    /// `NSTerminateLater` is returnable only from a variant holding a live
    /// receiver — the pairing the deadlock needed no longer exists.
    #[test]
    fn a_quit_is_only_deferred_when_a_worker_can_answer_it() {
        let root = scratch("quit");

        for phase in [
            Phase::idle(),
            Phase::idle().arm("us.zoom.xos".into()),
            Phase::Failed {
                dir: None,
                error: "the models did not load".into(),
                at: Local::now(),
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

        // Recording, stop succeeds: deferred, and the phase that answers owns
        // the receiver.
        let (phase, _tx) = recording(&root, None, Declined::default());
        let (after, q) = phase.on_quit(false);
        assert!(matches!(q, Quit::Later(_)));
        assert_eq!(after.kind(), PhaseKind::Stopping);
        assert!(after.live().is_some());

        // Stopping: deferred by construction.
        let (after, q) = after.on_quit(false);
        assert!(matches!(q, Quit::Later(_)));
        assert!(after.live().is_some());

        // Recording, stop fails: refused, and still recording.
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

    /// One entry to `Failed`, and two ways out.
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

        // Exit one: the banner is cleared.
        assert_eq!(failed.dismiss().kind(), PhaseKind::Idle);

        // Exit two: the watcher treats it as Idle, so arming dismisses it.
        let (phase, _tx) = recording(&root, None, Declined::default());
        let failed = phase.finished(Err(anyhow!("boom")));
        assert!(
            failed.kind().is_watching(),
            "Failed must not make the app deaf"
        );
        assert_eq!(failed.arm("us.zoom.xos".into()).kind(), PhaseKind::Armed);

        std::fs::remove_dir_all(&root).ok();
    }

    /// Declines are threaded by move through every variant, which is the half
    /// of bug 3 the consent rule alone cannot fix.
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

    /// Stopping records an answer about the call, or the watcher restarts the
    /// recording four seconds later and there is no way out until the call ends.
    #[test]
    fn stopping_declines_the_call_it_was_recording() {
        let root = scratch("declinecall");
        let watched = vec!["us.zoom.xos".to_string()];

        // Nothing is playing any more: fall back to the app the recording
        // belongs to.
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

    /// The half of issue #5 the quit table has to carry: with no capture
    /// running but a transcript still being written, quitting waits for the
    /// queue rather than abandoning it.
    #[test]
    fn a_busy_queue_defers_a_quit_from_every_workerless_phase() {
        let root = scratch("quitqueue");
        for phase in [
            Phase::idle(),
            Phase::idle().arm("us.zoom.xos".into()),
            Phase::Failed {
                dir: None,
                error: "boom".into(),
                at: Local::now(),
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
        // And with a capture running too, the answer is the same one as before.
        let (phase, _tx) = recording(&root, None, Declined::default());
        let (after, q) = phase.on_quit(true);
        assert!(matches!(q, Quit::Later(_)));
        assert_eq!(after.kind(), PhaseKind::Stopping);
        std::fs::remove_dir_all(&root).ok();
    }

    /// A transcript failing is drawn like any other failure when the app is
    /// idle, and stays out of the way when it is not.
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
