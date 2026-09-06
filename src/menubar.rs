//! The menu bar app.
//!
//! `NSApplication` owns the main thread, so capture runs on a worker — verified
//! rather than assumed, because a tap that is denied returns silence instead of
//! an error. A tap started on a spawned thread was measured at 0.657 on the call
//! channel, against 0.000 for the launch path that genuinely lacks the grant.
//!
//! Nothing here reaches into the capture layer. The worker runs the same
//! blocking `session::capture_into` — the first half of what the CLI's `record`
//! runs; the second half goes to the transcription queue — and the two halves
//! talk through the `STOP` sentinel and `status` file that already existed for
//! the detached bundle launch — which turn out to be exactly the interface a
//! GUI wants.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertStyle, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSApplicationTerminateReply, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{
    MainThreadMarker, NSObject, NSObjectProtocol, NSRunLoop, NSRunLoopCommonModes, NSString,
    NSTimer,
};

use crate::queue::Queue;
use crate::session::SessionDir;
use crate::state::{Live, Phase, PhaseCell, PhaseKind, Quit};

/// Which icon the status item shows. The phase's own symbol, except that an
/// `Idle` app with transcripts still being written is not idle to look at:
/// the hourglass says work is happening without the menu being opened.
fn symbol_for(kind: PhaseKind, transcribing: bool) -> &'static str {
    if kind == PhaseKind::Idle && transcribing {
        PhaseKind::Stopping.symbol()
    } else {
        kind.symbol()
    }
}

/// What the watcher should do about what is currently playing.
#[derive(Debug, PartialEq)]
enum Tick {
    Nothing,
    /// Stand down from Armed: the app it was waiting on has stopped, whether
    /// or not anything else is still playing.
    Disarm,
    Arm(String),
    Start(String),
}

/// The consent rule, kept pure so it can be tested without a menu bar.
///
/// An empty `apps` watches nothing. That list already means "capture all system
/// audio", and arming on any sound would flap at every notification chime — so
/// the Start item stays the only way in when nothing is named.
fn next(phase: &Phase, apps: &[String], live: &[String], ask: bool) -> Tick {
    if apps.is_empty() {
        return Tick::Nothing;
    }
    // Keyed on the armed app, not on whether anything at all is still
    // playing: a music player left running all day must not hold the prompt
    // open after the call it armed for has ended.
    if let Phase::Armed { app, .. } = phase {
        if !live.iter().any(|l| l == app) {
            return Tick::Disarm;
        }
    }
    // Skip past anything still declined rather than stopping at it: a watched
    // app left playing all day would otherwise mask every real call behind it.
    let declined = phase.declined();
    let Some(app) = apps
        .iter()
        .filter(|a| live.iter().any(|l| l == *a))
        .find(|a| !declined.is_declined(a))
    else {
        return Tick::Nothing;
    };
    // `Failed` is watched exactly like `Idle`: a failure the user has not
    // dismissed must not stop the next call being noticed.
    if !matches!(phase.kind(), PhaseKind::Idle | PhaseKind::Failed) {
        return Tick::Nothing;
    }
    if ask {
        Tick::Arm(app.clone())
    } else {
        Tick::Start(app.clone())
    }
}

struct Ivars {
    status_item: Retained<NSStatusItem>,
    /// The single source of truth. Nothing else here says what the app is
    /// doing, and the recording's receiver lives inside it — so a deferred
    /// quit and something alive to answer it are the same fact.
    phase: PhaseCell,
    /// The most recent failure, in the words it will be shown in. Step 5's
    /// banner reads this; until then the menu's status line does.
    banner: RefCell<Option<String>>,
    quitting: Cell<bool>,
    /// Sessions whose audio is written and whose transcript is not. Separate
    /// from the phase on purpose: the phase is `Idle` while this works, which
    /// is what lets the next call be recorded while the last is transcribed.
    queue: RefCell<Queue>,
    /// A transcript that failed while the phase was busy with something
    /// else. `Failed` is entered only from `Idle`, and the banner is drawn
    /// only there, so a failure that lands mid-recording waits here and is
    /// applied on the first idle tick — rather than being a log line the
    /// user never sees.
    pending_failure: RefCell<Option<(PathBuf, anyhow::Error)>>,
    log: PathBuf,
    /// The session browser — and, since the settings page moved into it, the
    /// app's only window. Held across opens so it and the settings bridge
    /// survive being closed. Built on first open rather than at launch: the
    /// app spends most of its life with nobody reading a transcript.
    main_window: RefCell<Option<crate::window::MainWindow>>,
    start_item: RefCell<Option<Retained<NSMenuItem>>>,
    stop_item: RefCell<Option<Retained<NSMenuItem>>>,
    level_item: RefCell<Option<Retained<NSMenuItem>>>,
    /// What the transcription queue is doing, under the level line. Hidden
    /// when the queue is empty.
    queue_item: RefCell<Option<Retained<NSMenuItem>>>,
    record_call_item: RefCell<Option<Retained<NSMenuItem>>>,
    decline_item: RefCell<Option<Retained<NSMenuItem>>>,
    /// The refresh timer runs at 2 Hz for the level meter; watching for calls
    /// needs nothing like that rate, so it happens every eighth tick.
    ticks: Cell<u64>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AmbientDelegate"]
    #[ivars = Ivars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl NSApplicationDelegate for Delegate {
        /// Quitting during a recording used to kill the worker outright,
        /// abandoning the audio and leaving scratch files that look live for
        /// ever. Stop it properly and let the transcription finish — and if
        /// the stop itself fails, refuse the quit rather than walking away
        /// from a capture that is still running.
        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _app: &NSApplication) -> NSApplicationTerminateReply {
            // The whole table lives in `Phase::on_quit`. `Later` is
            // unconstructible outside a variant that owns the receiver which
            // will answer it, so the pairing bug 1 needed — a deferred quit
            // with nothing alive to reply — cannot be expressed here.
            // Read before the transition: the closure is pure, and the queue
            // is a second thing that can answer a deferred quit.
            let queue_busy = !self.ivars().queue.borrow().is_empty();
            let reply = self.ivars().phase.transition(|p| p.on_quit(queue_busy));
            self.render();
            match reply {
                Quit::Now => NSApplicationTerminateReply::TerminateNow,
                Quit::Later(_) => {
                    self.log("quit requested — finishing the recording first");
                    self.ivars().quitting.set(true);
                    // `refresh` replies once the worker hands back a result.
                    NSApplicationTerminateReply::TerminateLater
                }
                Quit::Cancel(e) => {
                    self.fail("could not stop the recording", &e);
                    self.alert(
                        "Ambient is still recording",
                        &format!(
                            "The recording could not be stopped, so quitting now would \
                             abandon it.\n\n{e}\n\nThe capture is still running. Try Stop \
                             Recording again, or check the sessions folder."
                        ),
                    );
                    NSApplicationTerminateReply::TerminateCancel
                }
            }
        }

        /// **False, or the flip is worse than not flipping.** The window is a
        /// reader for a background agent, and closing a reader must not kill
        /// the agent — an app that stopped recording your calls because you
        /// closed a transcript would be the worst bug in the program.
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn terminate_after_last_window(&self, _app: &NSApplication) -> bool {
            false
        }

        /// Clicking the Dock icon, or `open -a Ambient`, while the app is
        /// already running. Without this the icon the promotion just put in
        /// the Dock does nothing at all when clicked.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, _visible: bool) -> bool {
            self.present_window(false);
            true
        }
    }

    impl Delegate {
        #[unsafe(method(startRecording:))]
        fn start_recording(&self, _sender: Option<&AnyObject>) {
            if self.ivars().phase.snapshot().has_worker {
                return;
            }
            self.log("start recording");
            self.begin_recording(None);
        }

        /// The armed state's yes. Identical to pressing Start, but logged
        /// differently because consent given to a specific call is worth being
        /// able to point at afterwards.
        #[unsafe(method(recordThisCall:))]
        fn record_this_call(&self, _sender: Option<&AnyObject>) {
            let armed = self.ivars().phase.with(|p| match p.kind() {
                PhaseKind::Armed => p.app(),
                _ => None,
            });
            let Some(who) = armed else { return };
            self.log(&format!("recording {who} — allowed by the user"));
            self.begin_recording(Some(who));
        }

        /// The armed state's no. Remembered against the bundle so the menu does
        /// not ask again two seconds later, and forgotten once that app goes
        /// quiet.
        #[unsafe(method(notThisOne:))]
        fn not_this_one(&self, _sender: Option<&AnyObject>) {
            let who = self.ivars().phase.with(|p| match p.kind() {
                PhaseKind::Armed => p.app(),
                _ => None,
            });
            let Some(who) = who else { return };
            self.log(&format!("declined {who} — not recording"));
            self.ivars().phase.transition(|mut p| {
                p.declined_mut().decline(&who);
                (p.disarm(), ())
            });
            self.render();
        }

        #[unsafe(method(stopRecording:))]
        fn stop_recording(&self, _sender: Option<&AnyObject>) {
            if self.ivars().phase.snapshot().kind != PhaseKind::Recording {
                return;
            }
            self.log("stop requested");
            // Read before the transition: the closure is pure, and neither of
            // these can be asked for while the phase is out of its cell.
            let cfg = crate::config::Config::load();
            let playing = crate::probe::bundles_rendering_output();
            // The sentinel goes into the directory this recording claimed, so
            // changing the sessions folder mid-recording no longer makes the
            // running capture unreachable.
            let failure = self.ivars().phase.transition(|p| {
                match p.decline_this_call(&cfg.apps, &playing).stop() {
                    Ok(next) => (next, None),
                    Err((back, e)) => (back, Some(e)),
                }
            });
            if let Some(e) = failure {
                // Still Recording, because the worker is still recording.
                // Stop stays enabled and Start stays disabled.
                self.fail("could not stop the recording", &e);
            }
            self.render();
        }

        /// Clear a failure banner. Reachable only from the page's Dismiss
        /// button — there is no menu item for it.
        #[unsafe(method(dismissFailure:))]
        fn dismiss_failure(&self, _sender: Option<&AnyObject>) {
            self.log("failure dismissed");
            self.ivars().phase.transition(|p| (p.dismiss(), ()));
            self.render();
        }

        /// Settings is a row in the one window now, not a window of its own.
        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            self.present_window(true);
        }

        /// Open the session browser. This replaces Open Sessions Folder,
        /// which was standing in for a browser that now exists — the folder
        /// is one Reveal in Finder away inside the window.
        #[unsafe(method(openWindow:))]
        fn open_window(&self, _sender: Option<&AnyObject>) {
            self.present_window(false);
        }

        #[unsafe(method(tick:))]
        fn tick(&self, _sender: Option<&AnyObject>) {
            self.refresh();
        }
    }
);

impl Delegate {
    fn log(&self, msg: &str) {
        eprintln!("{msg}");
        let line = format!("{}  {msg}\n", chrono::Local::now().to_rfc3339());
        if let Some(parent) = self.ivars().log.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.ivars().log)
        {
            use std::io::Write;
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// The one place a failure is recorded: the log line, and the words the
    /// banner will use. `eprintln!` goes nowhere under `open -a`, so a failure
    /// that only reaches stderr has not been reported at all.
    fn fail(&self, ctx: &str, e: &anyhow::Error) {
        let msg = format!("{ctx}: {e:#}");
        self.log(&format!("FAILED — {msg}"));
        *self.ivars().banner.borrow_mut() = Some(msg);
    }

    /// A modal the user cannot miss. Used only where carrying on would lose a
    /// recording, because an Accessory app interrupting anything else is rude.
    fn alert(&self, title: &str, body: &str) {
        let mtm = MainThreadMarker::from(self);
        let a = NSAlert::new(mtm);
        a.setAlertStyle(NSAlertStyle::Critical);
        a.setMessageText(&NSString::from_str(title));
        a.setInformativeText(&NSString::from_str(body));
        NSApplication::sharedApplication(mtm).activate();
        a.runModal();
    }

    /// Open the one window, promoting the app to `Regular` on the way.
    ///
    /// The **only** promoter, and every caller of it is an explicit user
    /// action: Open Ambient, Settings…, or a click on the Dock icon. A call
    /// arriving, a recording starting and a transcript finishing all go
    /// through `render`, which never touches the policy — a background event
    /// that put Ambient in front of the meeting you are in would be worse
    /// than having no window at all.
    fn present_window(&self, settings: bool) {
        let mtm = MainThreadMarker::from(self);
        {
            let mut slot = self.ivars().main_window.borrow_mut();
            if slot.is_none() {
                *slot = Some(crate::window::MainWindow::open(mtm));
            }
        }
        // Order front first, then paint: `render` skips a window that is not
        // visible, and both happen inside one turn of the run loop, so nothing
        // is drawn in between.
        if let Some(w) = self.ivars().main_window.borrow().as_ref() {
            w.show(mtm);
            if settings {
                w.select_settings();
            }
        }
        self.render();
        if let Some(w) = self.ivars().main_window.borrow().as_ref() {
            self.log(&w.describe_state());
        }
    }

    /// Shared by the Start item, the armed state's yes, and the watcher.
    ///
    /// The directory is claimed here, on the main thread, before the worker
    /// exists — so the phase knows where the recording is writing rather than
    /// having to go and find it later.
    fn begin_recording(&self, app: Option<String>) {
        let dir = match SessionDir::claim(&crate::session::home(), None) {
            Ok(d) => Arc::new(d),
            Err(e) => {
                self.fail("could not start a recording", &e);
                // Stand down rather than sit Armed over a recording that was
                // never spawned.
                self.ivars().phase.transition(|p| (p.disarm(), ()));
                self.render();
                return;
            }
        };
        let (tx, rx) = channel();
        let worker = dir.clone();
        let meter = Arc::new(crate::session::Meter::default());
        let worker_meter = meter.clone();
        std::thread::spawn(move || {
            // The same blocking function the CLI runs. ProcessTap is created
            // and dropped on this thread and never moves.
            tx.send(crate::session::capture_into(
                &worker,
                None,
                &[],
                None,
                None,
                Some(worker_meter),
            ))
            .ok();
        });
        let live = Live {
            dir,
            started: Instant::now(),
            // Kept: stopping needs to know which call this recording belongs to.
            app,
            result: rx,
            meter,
        };
        let orphan = self.ivars().phase.transition(|p| p.start(live));
        if let Some(live) = orphan {
            // Unreachable: the phase was checked on this same thread a few
            // lines up. Say so, and stop the worker rather than dropping its
            // receiver and leaving a capture nobody owns.
            crate::session::signal_stop(&live.dir).ok();
            self.log("internal error: a recording started into a busy phase — stopped again");
        }
        self.render();
    }

    /// Notice a watched app starting to produce audio, and act on `next`.
    fn poll_for_calls(&self) {
        let cfg = crate::config::Config::load();
        let playing = crate::probe::bundles_rendering_output();
        let tick = self.ivars().phase.transition(|mut p| {
            // Unconditional, every poll: a "not this one" answers only for as
            // long as that call is still going.
            p.declined_mut().retire(&playing);
            let tick = next(&p, &cfg.apps, &playing, cfg.ask_before_recording);
            let p = match &tick {
                Tick::Disarm => p.disarm(),
                Tick::Arm(app) => p.arm(app.clone()),
                // `Start` needs a worker, which is not something a pure
                // closure may spawn. Handled below.
                Tick::Nothing | Tick::Start(_) => p,
            };
            (p, tick)
        });
        match tick {
            Tick::Nothing => {}
            Tick::Disarm => self.render(),
            Tick::Arm(app) => {
                self.log(&format!("{app} is producing audio — waiting to be told"));
                self.render();
            }
            Tick::Start(app) => {
                self.log(&format!("{app} is producing audio — recording"));
                self.begin_recording(Some(app));
            }
        }
    }

    /// The only writer of the menu. Every control's title, enabled and hidden
    /// flag comes from the phase and from nowhere else; a control set anywhere
    /// but here is a future desync.
    fn render(&self) {
        let view = self.ivars().phase.snapshot();
        let queue_line = self.ivars().queue.borrow().summary();
        let mtm = MainThreadMarker::from(self);
        {
            if let Some(button) = self.ivars().status_item.button(mtm) {
                let name = NSString::from_str(symbol_for(view.kind, queue_line.is_some()));
                let desc = NSString::from_str("Ambient");
                if let Some(img) =
                    NSImage::imageWithSystemSymbolName_accessibilityDescription(&name, Some(&desc))
                {
                    img.setTemplate(true);
                    button.setImage(Some(&img));
                }
            }
        }
        let armed = view.kind == PhaseKind::Armed;
        let failed = view.kind == PhaseKind::Failed;
        if let Some(i) = self.ivars().start_item.borrow().as_ref() {
            i.setEnabled(!view.has_worker);
        }
        if let Some(i) = self.ivars().stop_item.borrow().as_ref() {
            // Stays enabled through a failed stop, because the worker is still
            // recording and pressing Stop again is the right thing to do.
            i.setEnabled(view.kind == PhaseKind::Recording);
        }
        if let Some(i) = self.ivars().level_item.borrow().as_ref() {
            // Shown while a worker is running, and while a failure is standing:
            // a recording that produced audio and no transcript must not be
            // drawn as "nothing happened".
            i.setHidden(!(view.has_worker || failed));
            if failed {
                // The banner first: it holds the most recent failure, which
                // is not always the one that put the phase here — a claim
                // that failed over a standing `Failed` is newer news.
                let text = self
                    .ivars()
                    .banner
                    .borrow()
                    .clone()
                    .or_else(|| self.ivars().phase.with(|p| p.failure().map(str::to_string)))
                    .unwrap_or_else(|| "the last recording failed".into());
                i.setTitle(&NSString::from_str(&format!("failed: {text}")));
            } else if view.has_worker {
                // Read from the shared `Meter` rather than the session's
                // status file: there is no walk of the sessions folder left
                // to pick the wrong thing, and no free-text file to parse.
                let text = self
                    .ivars()
                    .phase
                    .with(|p| p.live().map(|l| l.meter.status_line()))
                    .unwrap_or_else(|| "starting…".into());
                i.setTitle(&NSString::from_str(&text));
            }
        }
        if let Some(i) = self.ivars().queue_item.borrow().as_ref() {
            match &queue_line {
                Some(text) => {
                    i.setTitle(&NSString::from_str(text));
                    i.setHidden(false);
                }
                None => i.setHidden(true),
            }
        }
        // The consent pair is the whole menu when it is showing: hidden the
        // rest of the time so the ordinary menu is not cluttered by a choice
        // nobody is being asked to make.
        for i in [
            self.ivars().record_call_item.borrow().as_ref(),
            self.ivars().decline_item.borrow().as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            i.setHidden(!armed);
        }
        if armed {
            if let (Some(i), Some(app)) = (
                self.ivars().record_call_item.borrow().as_ref(),
                self.ivars().phase.with(Phase::app),
            ) {
                i.setTitle(&NSString::from_str(&format!("Record {app}")));
            }
        }
        // The window is painted from the same phase in the same pass. It has
        // its own single renderer; this is the only place it is called.
        if let Some(w) = self.ivars().main_window.borrow().as_ref() {
            self.ivars()
                .phase
                .with(|p| w.render(p, queue_line.as_deref(), mtm));
        }
    }

    /// Called on a timer: repaint from the phase, and notice when the worker
    /// has finished.
    fn refresh(&self) {
        let view = self.ivars().phase.snapshot();

        // Every eighth tick, so roughly every four seconds. Enumerating audio
        // processes twice a second would be pure waste for something that
        // changes when a human joins a call.
        let n = self.ivars().ticks.get().wrapping_add(1);
        self.ivars().ticks.set(n);
        // Not while a quit is waiting on the queue: the phase is `Idle` then,
        // and a call starting must not begin a recording under an app that
        // has been asked to leave.
        if n % 8 == 0 && view.kind.is_watching() && !self.ivars().quitting.get() {
            self.poll_for_calls();
        }

        // Every tick, through the one renderer: the elapsed line moves while
        // nothing about the phase does, and a control set anywhere but
        // `render` is a future desync.
        self.render();

        // Retry a demotion that its own guards refused. `windowWillClose:` is
        // the only other caller, so without this the guards are one-shot: open
        // the About panel, close the main window (the guard refuses, correctly,
        // because demoting would strip About's menu bar), then close About, and
        // the app is left in Regular for ever — Dock icon and Cmd-Tab entry
        // with no window behind them. This is not a background flip: it
        // completes a close the user already asked for. Both guards still hold,
        // and the call is a no-op under Accessory, so a tick costs one
        // `activationPolicy()` read in the normal case.
        crate::window::demote_to_accessory(MainThreadMarker::from(self), None);

        let finished = self.ivars().phase.with(|p| {
            p.live().and_then(|l| match l.result.try_recv() {
                Ok(r) => Some(r),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Some(Err(anyhow::anyhow!("the recording thread went away")))
                }
            })
        });
        if let Some(result) = finished {
            let captured = match &result {
                Ok(dir) => {
                    self.log(&format!("audio written: {}", dir.display()));
                    Some(dir.clone())
                }
                Err(e) => {
                    self.fail("recording", e);
                    None
                }
            };
            // The phase is `Idle` from here: Start and the watcher both work
            // again while the transcript is written on the queue's thread.
            self.ivars().phase.transition(|p| (p.finished(result), ()));
            if let Some(dir) = captured {
                self.ivars().queue.borrow_mut().push(dir);
            }
            self.render();
        }

        let done = self.ivars().queue.borrow_mut().poll();
        if let Some((dir, result)) = done {
            match result {
                Ok(_) => self.log(&format!("transcript written: {}", dir.display())),
                Err(e) => {
                    self.fail(&format!("transcribing {}", dir.display()), &e);
                    // Held rather than applied: the phase may be busy with
                    // the next recording. The newest failure wins the slot;
                    // the log and each session's `status` file keep the rest.
                    *self.ivars().pending_failure.borrow_mut() = Some((dir, e));
                }
            }
            self.render();
        }

        // Apply a held failure on the first idle tick, so it is drawn the
        // way a capture failure is and can be dismissed the same way.
        if self.ivars().phase.snapshot().kind == PhaseKind::Idle {
            let held = self.ivars().pending_failure.borrow_mut().take();
            if let Some((dir, e)) = held {
                self.ivars()
                    .phase
                    .transition(|p| (p.transcript_failed(dir, &e), ()));
                self.render();
            }
        }

        // A deferred quit is answered once nothing is left to wait for: no
        // capture worker, and nothing on the queue. Checked every tick rather
        // than only when something finishes, so the order the two empty in
        // does not matter.
        if self.ivars().quitting.get()
            && !self.ivars().phase.snapshot().has_worker
            && self.ivars().queue.borrow().is_empty()
        {
            self.ivars().quitting.set(false);
            let mtm = MainThreadMarker::from(self);
            NSApplication::sharedApplication(mtm).replyToApplicationShouldTerminate(true);
        }
    }
}

/// `~/Library/Logs/Ambient/app.log`. Out of the sessions folder on purpose:
/// `app.log` sorts after every `2…` session id, so any walk that forgets to
/// filter picks the log up as the newest session — and the sessions folder
/// should hold only sessions.
fn log_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("Library")
        .join("Logs")
        .join("Ambient")
        .join("app.log")
}

fn item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<objc2::runtime::Sel>,
    key: &str,
) -> Retained<NSMenuItem> {
    let t = NSString::from_str(title);
    let k = NSString::from_str(key);
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &t, action, &k)
    }
}

/// Start the menu bar app. Does not return until the user quits.
pub fn run() -> anyhow::Result<()> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| anyhow::anyhow!("the app must be started on the main thread"))?;
    let app = NSApplication::sharedApplication(mtm);
    // Accessory: lives in the menu bar, keeps out of the Dock and the switcher.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // A real application main menu, installed before any window exists. It is
    // inert under Accessory and mandatory under Regular: promoting the policy
    // without it yields a menu bar holding only the Apple menu.
    crate::window::install_main_menu(mtm);

    let status_item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);

    let delegate = Delegate::alloc(mtm).set_ivars(Ivars {
        status_item: status_item.clone(),
        phase: PhaseCell::new(Phase::idle()),
        banner: RefCell::new(None),
        quitting: Cell::new(false),
        // One worker thread for the life of the app. The default model, as
        // the CLI's `record` uses; there is no flag to pass here.
        queue: RefCell::new(Queue::spawn(|dir, meter| {
            crate::session::transcribe_session(dir, None, Some(meter.clone()))
        })),
        pending_failure: RefCell::new(None),
        // Launched from Finder there is nowhere for stderr to go, so keep our
        // own log — the last failure was invisible for exactly this reason.
        log: log_path(),
        main_window: RefCell::new(None),
        start_item: RefCell::new(None),
        stop_item: RefCell::new(None),
        level_item: RefCell::new(None),
        queue_item: RefCell::new(None),
        record_call_item: RefCell::new(None),
        decline_item: RefCell::new(None),
        ticks: Cell::new(0),
    });
    let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };

    let menu = NSMenu::new(mtm);
    let record_call = item(mtm, "Record this call", Some(sel!(recordThisCall:)), "");
    let decline = item(mtm, "Not this one", Some(sel!(notThisOne:)), "");
    let start = item(mtm, "Start Recording", Some(sel!(startRecording:)), "r");
    let stop = item(mtm, "Stop Recording", Some(sel!(stopRecording:)), "s");
    let level = item(mtm, "", None, "");
    let queue_line = item(mtm, "", None, "");
    let settings = item(mtm, "Settings…", Some(sel!(openSettings:)), ",");
    let open = item(mtm, "Open Ambient", Some(sel!(openWindow:)), "0");
    let quit = item(mtm, "Quit Ambient", Some(sel!(terminate:)), "q");

    for i in [&start, &stop, &settings, &open, &record_call, &decline] {
        unsafe { i.setTarget(Some(&delegate)) };
    }
    {
        level.setEnabled(false);
        level.setHidden(true);
        queue_line.setEnabled(false);
        queue_line.setHidden(true);
        // Above Start, because when they are showing they are the decision the
        // menu was opened to make.
        record_call.setHidden(true);
        decline.setHidden(true);
        menu.addItem(&record_call);
        menu.addItem(&decline);
        menu.addItem(&start);
        menu.addItem(&stop);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&level);
        menu.addItem(&queue_line);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&open);
        menu.addItem(&settings);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&quit);
        status_item.setMenu(Some(&menu));
    }

    *delegate.ivars().start_item.borrow_mut() = Some(start);
    *delegate.ivars().stop_item.borrow_mut() = Some(stop);
    *delegate.ivars().level_item.borrow_mut() = Some(level);
    *delegate.ivars().queue_item.borrow_mut() = Some(queue_line);
    *delegate.ivars().record_call_item.borrow_mut() = Some(record_call);
    *delegate.ivars().decline_item.borrow_mut() = Some(decline);
    // Anything a crash left captured but untranscribed goes straight back on
    // the queue: the split means that state can exist, so the app must be
    // able to finish it, and the audio is already on disk waiting.
    for dir in crate::session::captured_awaiting_transcript(&crate::session::home()) {
        delegate.log(&format!("re-queued for transcription: {}", dir.display()));
        delegate.ivars().queue.borrow_mut().push(dir);
    }
    delegate.render();
    // The status item is the whole app; say plainly whether the system gave us
    // one rather than leaving an empty menu bar to be interpreted.
    eprintln!(
        "menu bar item: {}",
        match status_item.button(mtm) {
            Some(_) => "ready",
            None => "NOT CREATED",
        }
    );

    // Poll twice a second: fast enough for a level meter, cheap enough to
    // ignore. `record` rewrites its status line once a second.
    let d = delegate.clone();
    let block = RcBlock::new(move |_t: core::ptr::NonNull<NSTimer>| d.refresh());
    // Added in the common modes rather than scheduled: `scheduledTimer…`
    // registers only for NSDefaultRunLoopMode, and AppKit runs the loop in
    // event-tracking mode for as long as a menu is open. The elapsed time
    // therefore froze exactly while you were looking at it, and only moved
    // when the menu was closed and reopened.
    unsafe {
        let timer = NSTimer::timerWithTimeInterval_repeats_block(0.5, true, &block);
        NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
    }
    std::mem::forget(block);

    let object = ProtocolObject::from_ref(&*delegate);
    app.setDelegate(Some(object));
    std::mem::forget(delegate);
    app.run();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::{recording, scratch};
    use crate::state::Declined;

    fn apps() -> Vec<String> {
        vec!["us.zoom.xos".into(), "com.microsoft.teams2".into()]
    }

    fn declined(apps: &[&str]) -> Declined {
        let mut d = Declined::default();
        for a in apps {
            d.decline(a);
        }
        d
    }

    fn idle(d: Declined) -> Phase {
        Phase::Idle { declined: d }
    }

    fn armed(app: &str, d: Declined) -> Phase {
        Phase::Armed {
            app: app.into(),
            declined: d,
        }
    }

    /// The rule that matters most: with nothing named, nothing is ever armed.
    /// An empty list means "all system audio", so arming on it would ask about
    /// every notification chime.
    #[test]
    fn an_empty_watch_list_never_arms() {
        let live = vec!["us.zoom.xos".to_string()];
        assert_eq!(
            next(&idle(Declined::default()), &[], &live, true),
            Tick::Nothing
        );
    }

    #[test]
    fn a_watched_app_playing_arms_when_asked_to_ask() {
        let live = vec!["us.zoom.xos".to_string()];
        assert_eq!(
            next(&idle(Declined::default()), &apps(), &live, true),
            Tick::Arm("us.zoom.xos".into())
        );
    }

    #[test]
    fn with_asking_off_it_records_by_itself() {
        let live = vec!["com.microsoft.teams2".to_string()];
        assert_eq!(
            next(&idle(Declined::default()), &apps(), &live, false),
            Tick::Start("com.microsoft.teams2".into())
        );
    }

    #[test]
    fn an_unwatched_app_playing_is_ignored() {
        let live = vec!["com.spotify.client".to_string()];
        assert_eq!(
            next(&idle(Declined::default()), &apps(), &live, true),
            Tick::Nothing
        );
    }

    /// Declining must actually stick, or the menu asks again four seconds later
    /// and the answer means nothing.
    #[test]
    fn a_declined_call_is_not_asked_about_again() {
        let live = vec!["us.zoom.xos".to_string()];
        assert_eq!(
            next(&idle(declined(&["us.zoom.xos"])), &apps(), &live, true),
            Tick::Nothing
        );
    }

    /// ...but declining one call must not opt out of the next one: once the
    /// app falls quiet, `retire` drops it from the set.
    #[test]
    fn the_decline_is_forgotten_once_the_call_ends() {
        let mut d = declined(&["us.zoom.xos"]);
        d.retire(&[]);
        assert!(!d.is_declined("us.zoom.xos"));
        assert_eq!(next(&idle(d), &apps(), &[], true), Tick::Nothing);
    }

    #[test]
    fn declining_one_app_does_not_silence_another() {
        let live = vec!["com.microsoft.teams2".to_string()];
        assert_eq!(
            next(&idle(declined(&["us.zoom.xos"])), &apps(), &live, true),
            Tick::Arm("com.microsoft.teams2".into())
        );
    }

    /// A watched app left playing all day (a music player on the list) must
    /// not hide every real call behind it once it has been declined.
    #[test]
    fn a_declined_app_does_not_mask_a_later_call() {
        let live = vec![
            "us.zoom.xos".to_string(),
            "com.microsoft.teams2".to_string(),
        ];
        assert_eq!(
            next(&idle(declined(&["us.zoom.xos"])), &apps(), &live, true),
            Tick::Arm("com.microsoft.teams2".into())
        );
    }

    #[test]
    fn everything_playing_being_declined_asks_nothing() {
        let live = vec!["us.zoom.xos".to_string()];
        assert_eq!(
            next(&idle(declined(&["us.zoom.xos"])), &apps(), &live, true),
            Tick::Nothing
        );
    }

    #[test]
    fn a_call_ending_while_armed_stands_down() {
        assert_eq!(
            next(
                &armed("us.zoom.xos", Declined::default()),
                &apps(),
                &[],
                true
            ),
            Tick::Disarm
        );
    }

    /// A recording in progress must never be disturbed by the watcher, and
    /// nor must one still writing its audio after Stop.
    #[test]
    fn recording_and_stopping_are_left_alone() {
        let root = scratch("watcher-busy");
        let live = vec!["us.zoom.xos".to_string()];

        let (rec, _tx) = recording(&root, Some("us.zoom.xos"), Declined::default());
        let stopping = {
            let (r, _tx2) = recording(&root, Some("us.zoom.xos"), Declined::default());
            r.stop().unwrap_or_else(|(_, e)| panic!("{e}"))
        };
        for phase in [rec, stopping, armed("us.zoom.xos", Declined::default())] {
            assert_eq!(next(&phase, &apps(), &live, true), Tick::Nothing);
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// Issue #5's other half: once the audio is written the phase is `Idle`,
    /// and the watcher must notice the next call even though the last one is
    /// still being transcribed. Transcription is the queue's business, not
    /// the phase's, so `next` cannot even see it.
    #[test]
    fn a_session_being_transcribed_does_not_deafen_the_watcher() {
        let root = scratch("watcher-queue");
        let live = vec!["us.zoom.xos".to_string()];
        let (rec, _tx) = recording(&root, Some("us.zoom.xos"), Declined::default());
        let idle = rec
            .stop()
            .unwrap_or_else(|(_, e)| panic!("{e}"))
            .finished(Ok(root.join("2026-09-02T0900")));
        assert_eq!(idle.kind(), PhaseKind::Idle);
        assert_eq!(
            next(&idle, &apps(), &live, true),
            Tick::Arm("us.zoom.xos".into())
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The icon says work is happening while the menu is closed.
    #[test]
    fn an_idle_app_with_a_busy_queue_shows_the_hourglass() {
        assert_eq!(symbol_for(PhaseKind::Idle, true), "hourglass");
        assert_eq!(symbol_for(PhaseKind::Idle, false), "waveform");
        // Every other state keeps its own icon: a call being asked about or
        // recorded is more important to show than a transcript being written.
        for kind in [PhaseKind::Armed, PhaseKind::Recording, PhaseKind::Failed] {
            assert_eq!(symbol_for(kind, true), kind.symbol());
        }
    }

    /// `Failed` is watched exactly like `Idle`, or an undismissed failure
    /// leaves the app deaf to every later call.
    #[test]
    fn a_standing_failure_does_not_make_the_watcher_deaf() {
        let live = vec!["us.zoom.xos".to_string()];
        let failed = Phase::Failed {
            dir: None,
            error: "the models did not load".into(),
            at: chrono::Local::now(),
            declined: Declined::default(),
        };
        assert_eq!(
            next(&failed, &apps(), &live, true),
            Tick::Arm("us.zoom.xos".into())
        );
    }

    /// Two watched apps playing: declining one must leave the other free to
    /// arm, and the decline must survive across ticks rather than being
    /// clobbered by the arm.
    #[test]
    fn declining_one_of_two_leaves_the_other_armable_and_sticky() {
        let live = vec![
            "us.zoom.xos".to_string(),
            "com.microsoft.teams2".to_string(),
        ];
        let d = declined(&["us.zoom.xos"]);

        assert_eq!(
            next(&idle(d.clone()), &apps(), &live, true),
            Tick::Arm("com.microsoft.teams2".into())
        );
        // Arm for teams and poll again: zoom must still read as declined,
        // because the set moves into the new phase rather than being rebuilt.
        let phase = idle(d).arm("com.microsoft.teams2".into());
        assert_eq!(next(&phase, &apps(), &live, true), Tick::Nothing);
        assert!(phase.declined().is_declined("us.zoom.xos"));
    }

    /// Both watched apps declined: nothing arms, and nothing re-prompts on a
    /// later tick with the same apps still playing.
    #[test]
    fn declining_both_arms_nothing_and_never_reprompts() {
        let live = vec![
            "us.zoom.xos".to_string(),
            "com.microsoft.teams2".to_string(),
        ];
        let d = declined(&["us.zoom.xos", "com.microsoft.teams2"]);
        for _ in 0..3 {
            assert_eq!(next(&idle(d.clone()), &apps(), &live, true), Tick::Nothing);
        }
    }

    /// The armed app falls quiet while a different watched app keeps playing:
    /// the phase must return to Idle rather than staying armed because
    /// *something* is still playing.
    #[test]
    fn armed_app_going_quiet_disarms_even_if_another_still_plays() {
        let live = vec!["com.microsoft.teams2".to_string()];
        assert_eq!(
            next(
                &armed("us.zoom.xos", Declined::default()),
                &apps(),
                &live,
                true
            ),
            Tick::Disarm
        );
    }
}
