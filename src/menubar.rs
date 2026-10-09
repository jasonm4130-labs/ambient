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
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly, Message};
use objc2_app_kit::{
    NSAlert, NSAlertStyle, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSApplicationTerminateReply, NSBeep, NSControlStateValueOff, NSControlStateValueOn, NSImage,
    NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
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
    /// The live assistant's on/off switch, ticked when it is on.
    assistant_item: RefCell<Option<Retained<NSMenuItem>>>,
    /// "AI assistant listening", shown while an agent is watching a meeting
    /// through `ambient mcp`: the on-screen half of its consent notice.
    assistant_line: RefCell<Option<Retained<NSMenuItem>>>,
    /// The assistant setting and heartbeat as last read. Both are files, so
    /// they are read every fourth tick rather than on every repaint.
    assistant_on: Cell<bool>,
    assistant_state: RefCell<Option<String>>,
    /// The refresh timer runs at 2 Hz for the level meter; watching for calls
    /// needs nothing like that rate, so it happens every eighth tick.
    ticks: Cell<u64>,
    /// Talking to firstmate: the hold, the turns and their workers.
    talk: RefCell<crate::talk::app::Talk>,
    /// The talk settings as last read, every fourth tick like the
    /// assistant's.
    talk_cfg: RefCell<crate::config::TalkConfig>,
    /// ⌥Space, held while talking mode is on.
    hotkey: RefCell<Option<crate::hotkey::HotKey>>,
    /// Why ⌥Space could not be had, shown until talking mode is turned off
    /// and on again.
    hotkey_error: RefCell<Option<String>>,
    /// "Hold to talk to firstmate ⌥Space", which also starts and sends a
    /// turn by click for anyone not holding the key.
    talk_item: RefCell<Option<Retained<NSMenuItem>>>,
    /// "Speak firstmate's replies", ticked when replies are read aloud.
    talk_speak_item: RefCell<Option<Retained<NSMenuItem>>>,
    /// The last reply, or where the current turn has got to.
    talk_line: RefCell<Option<Retained<NSMenuItem>>>,
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
            // A turn is not worth keeping the app for: its note is already
            // with firstmate, and the reply stays in firstmate's inbox.
            {
                let mut talk = self.ivars().talk.borrow_mut();
                talk.hold = None;
                talk.stop_all();
            }
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

        /// Turn the live assistant on or off. The switch is the setting
        /// itself: `ambient mcp` reads it on every watching call, so turning
        /// it off here silences an assistant mid-meeting.
        #[unsafe(method(toggleAssistant:))]
        fn toggle_assistant(&self, _sender: Option<&AnyObject>) {
            let mut cfg = crate::config::Config::load();
            cfg.assistant.enabled = !cfg.assistant.enabled;
            match cfg.save() {
                Ok(()) => {
                    self.log(&format!(
                        "live assistant turned {} by the user",
                        if cfg.assistant.enabled { "on" } else { "off" }
                    ));
                    self.ivars().assistant_on.set(cfg.assistant.enabled);
                }
                Err(e) => self.fail("could not change the assistant setting", &e),
            }
            self.render();
        }

        /// The window's talk button going down. ⌥Space comes in through
        /// [`Delegate::talk_key`] instead.
        #[unsafe(method(talkPress:))]
        fn talk_press_action(&self, _sender: Option<&AnyObject>) {
            self.talk_press();
        }

        #[unsafe(method(talkRelease:))]
        fn talk_release_action(&self, _sender: Option<&AnyObject>) {
            self.talk_release();
        }

        /// The menu item: a menu cannot be held, so a click starts listening
        /// and the next click sends.
        #[unsafe(method(talkToggle:))]
        fn talk_toggle(&self, _sender: Option<&AnyObject>) {
            if self.ivars().talk.borrow().listening() {
                self.talk_release();
            } else {
                self.talk_press();
            }
        }

        #[unsafe(method(toggleTalkSpeak:))]
        fn toggle_talk_speak(&self, _sender: Option<&AnyObject>) {
            let mut cfg = crate::config::Config::load();
            cfg.talk.speak = !cfg.talk.speak;
            match cfg.save() {
                Ok(()) => {
                    self.log(&format!(
                        "speaking firstmate's replies turned {} by the user",
                        if cfg.talk.speak { "on" } else { "off" }
                    ));
                    *self.ivars().talk_cfg.borrow_mut() = cfg.talk;
                }
                Err(e) => self.fail("could not change the talk setting", &e),
            }
            self.render();
        }

        #[unsafe(method(tick:))]
        fn tick(&self, _sender: Option<&AnyObject>) {
            self.refresh();
        }
    }
);

impl Delegate {
    fn log(&self, msg: &str) {
        append_log(&self.ivars().log, msg);
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
        // The microphone is the recording's now, and nothing may be said
        // into the meeting.
        self.pause_talk("A recording started, so talking stopped.");
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
                let talk_symbol = self.ivars().talk.borrow().symbol();
                let name = NSString::from_str(
                    talk_symbol.unwrap_or_else(|| symbol_for(view.kind, queue_line.is_some())),
                );
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
        if let Some(i) = self.ivars().assistant_item.borrow().as_ref() {
            i.setState(if self.ivars().assistant_on.get() {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
        if let Some(i) = self.ivars().assistant_line.borrow().as_ref() {
            match self.ivars().assistant_state.borrow().as_deref() {
                Some(state) => {
                    let text = if state == "speaking" {
                        "AI assistant speaking"
                    } else {
                        "AI assistant listening — it may speak"
                    };
                    i.setTitle(&NSString::from_str(text));
                    i.setHidden(false);
                }
                None => i.setHidden(true),
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
        self.render_talk(view.kind);
        // The window is painted from the same phase in the same pass. It has
        // its own single renderer; this is the only place it is called.
        if let Some(w) = self.ivars().main_window.borrow().as_ref() {
            self.ivars()
                .phase
                .with(|p| w.render(p, queue_line.as_deref(), mtm));
            let talk = self.ivars().talk.borrow().payload(
                &self.ivars().talk_cfg.borrow(),
                match self.ivars().hotkey_error.borrow().as_deref() {
                    Some(e) => Err(e),
                    None => Ok(()),
                },
                view.kind,
            );
            w.render_talk(&talk);
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
        if n % 4 == 0 {
            self.read_assistant();
            self.read_talk();
        }
        self.tick_talk(self.ivars().phase.snapshot().kind);

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

impl Delegate {
    /// Refresh the assistant setting and heartbeat the menu shows.
    fn read_assistant(&self) {
        let config = crate::config::path();
        self.ivars()
            .assistant_on
            .set(crate::config::Config::load_from(&config).assistant.enabled);
        *self.ivars().assistant_state.borrow_mut() = crate::assist::listening(&config);
    }

    /// Refresh the talk settings, and hold ⌥Space exactly while talking mode
    /// is on.
    fn read_talk(&self) {
        let cfg = crate::config::Config::load().talk;
        let held = self.ivars().hotkey.borrow().is_some();
        if cfg.enabled && !held && self.ivars().hotkey_error.borrow().is_none() {
            let me = self.retain();
            match crate::hotkey::HotKey::register(move |down| me.talk_key(down)) {
                Ok(k) => {
                    self.log(&format!("talking mode on — {} held", crate::hotkey::CHORD));
                    *self.ivars().hotkey.borrow_mut() = Some(k);
                }
                Err(e) => {
                    self.log(&format!("talking mode on, but {e:#}"));
                    *self.ivars().hotkey_error.borrow_mut() = Some(format!("{e:#}"));
                }
            }
        } else if !cfg.enabled {
            if self.ivars().hotkey.borrow_mut().take().is_some() {
                self.log(&format!(
                    "talking mode off — {} released",
                    crate::hotkey::CHORD
                ));
            }
            // Turning it off and on again is how to retry a refused chord.
            *self.ivars().hotkey_error.borrow_mut() = None;
            self.ivars().talk.borrow_mut().hold = None;
        }
        *self.ivars().talk_cfg.borrow_mut() = cfg;
    }

    /// ⌥Space went down (`true`) or came up.
    fn talk_key(&self, down: bool) {
        if down {
            self.talk_press();
        } else {
            self.talk_release();
        }
    }

    /// Open the microphone for a turn, unless talking is refused right now.
    fn talk_press(&self) {
        // Key repeat, or the window's button and the key at once.
        if self.ivars().talk.borrow().listening() {
            return;
        }
        let cfg = crate::config::Config::load();
        *self.ivars().talk_cfg.borrow_mut() = cfg.talk.clone();
        let kind = self.ivars().phase.snapshot().kind;
        if let Some(why) = crate::talk::app::refusal(&cfg.talk, kind) {
            self.log(&format!("talk refused — {why}"));
            self.ivars().talk.borrow_mut().notice(why);
            NSBeep();
            self.render();
            return;
        }
        // Holding the key again interrupts a reply being read out.
        self.ivars().talk.borrow_mut().barge_in();
        match crate::capture::MicHold::start(cfg.input_device.as_deref()) {
            Ok(hold) => self.ivars().talk.borrow_mut().hold = Some(hold),
            Err(e) => {
                self.fail("could not open the microphone to talk", &e);
                self.ivars()
                    .talk
                    .borrow_mut()
                    .notice(format!("Could not open the microphone: {e:#}"));
            }
        }
        self.render();
    }

    /// Hand what was heard to a talk worker. The samples go down its stdin
    /// and are dropped here.
    fn talk_release(&self) {
        let Some(hold) = self.ivars().talk.borrow_mut().hold.take() else {
            return;
        };
        let rate = hold.rate;
        let samples = hold.finish();
        let seconds = samples.len() as f64 / rate;
        let speak = self.ivars().talk_cfg.borrow().speak;
        if seconds < crate::talk::app::MIN_HOLD_S {
            self.ivars().talk.borrow_mut().notice(format!(
                "Hold {} while you speak, then let go.",
                crate::hotkey::CHORD
            ));
        } else {
            let spawned = self.ivars().talk.borrow_mut().spawn(samples, rate, speak);
            match spawned {
                Ok(id) => self.log(&format!(
                    "talk turn {id}: {seconds:.1} s handed to the worker"
                )),
                Err(e) => {
                    self.ivars()
                        .talk
                        .borrow_mut()
                        .notice(format!("Could not start talking: {e:#}"));
                    self.fail("could not start the talk worker", &e);
                }
            }
        }
        self.render();
    }

    /// Drop the hold and keep every open turn quiet, saying why.
    fn pause_talk(&self, why: &str) {
        let mut talk = self.ivars().talk.borrow_mut();
        let was_listening = talk.hold.take().is_some();
        let was_speaking = talk.speaking();
        crate::talk::set_paused(&crate::session::home(), true);
        talk.hush();
        if was_listening || was_speaking {
            talk.notice(why);
            drop(talk);
            self.log(&format!("talk paused — {why}"));
        }
    }

    /// Per tick: take in the workers' news, let go of a hold that has run
    /// its limit, and pause talking if a call is now waiting to be recorded.
    fn tick_talk(&self, kind: PhaseKind) {
        match crate::talk::app::pause_reason(kind) {
            Some(why) => self.pause_talk(why),
            None => crate::talk::set_paused(&crate::session::home(), false),
        }
        let over = self
            .ivars()
            .talk
            .borrow()
            .hold
            .as_ref()
            .is_some_and(|h| h.held().as_secs() as usize >= crate::capture::MicHold::MAX_S);
        if over {
            self.talk_release();
        }
        self.ivars().talk.borrow_mut().poll();
    }

    /// The talk items: shown only in talking mode.
    fn render_talk(&self, kind: PhaseKind) {
        let cfg = self.ivars().talk_cfg.borrow();
        let talk = self.ivars().talk.borrow();
        if let Some(i) = self.ivars().talk_item.borrow().as_ref() {
            i.setHidden(!cfg.enabled);
            let title = if talk.listening() {
                "Send to firstmate".to_string()
            } else {
                format!("Hold to talk to firstmate {}", crate::hotkey::CHORD)
            };
            i.setTitle(&NSString::from_str(&title));
            i.setEnabled(crate::talk::app::paused(kind).is_none());
        }
        if let Some(i) = self.ivars().talk_speak_item.borrow().as_ref() {
            i.setHidden(!cfg.enabled);
            i.setState(if cfg.speak {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
        if let Some(i) = self.ivars().talk_line.borrow().as_ref() {
            let line = match (
                self.ivars().hotkey_error.borrow().as_deref(),
                talk.menu_line(),
            ) {
                (_, Some(l)) => Some(l),
                (Some(e), None) => Some(format!("{} unavailable: {e}", crate::hotkey::CHORD)),
                (None, None) => None,
            };
            match line.filter(|_| cfg.enabled) {
                Some(l) => {
                    i.setTitle(&NSString::from_str(&l));
                    i.setHidden(false);
                }
                None => i.setHidden(true),
            }
        }
    }
}

/// One line to stderr and to the app log at `path`.
fn append_log(path: &Path, msg: &str) {
    eprintln!("{msg}");
    let line = format!("{}  {msg}\n", chrono::Local::now().to_rfc3339());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
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

    // One decoder lane across this process and the finalize children it
    // starts, so the live pass and the queue's transcriber stay one worker.
    crate::live::share_decoder(std::env::temp_dir().join("ambient-decoder.lock"));

    let delegate = Delegate::alloc(mtm).set_ivars(Ivars {
        status_item: status_item.clone(),
        phase: PhaseCell::new(Phase::idle()),
        banner: RefCell::new(None),
        quitting: Cell::new(false),
        // One worker thread for the life of the app, each job a child
        // process of this same executable, so what finalizing allocates goes
        // back to the system when the job ends rather than staying resident
        // in a menu bar app that runs all day. The default model, as the
        // CLI's `record` uses; there is no flag to pass here.
        queue: RefCell::new(Queue::spawn(|dir, meter| match std::env::current_exe() {
            Ok(exe) => crate::finalize::run(crate::finalize::command(&exe, dir), dir, meter),
            Err(e) => {
                append_log(
                    &log_path(),
                    &format!(
                        "warning: cannot locate own executable ({e}); finalizing in-process, \
                         which keeps its memory resident after the job"
                    ),
                );
                crate::session::transcribe_session(dir, None, Some(meter.clone()))
            }
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
        assistant_item: RefCell::new(None),
        assistant_line: RefCell::new(None),
        assistant_on: Cell::new(false),
        assistant_state: RefCell::new(None),
        ticks: Cell::new(0),
        talk: RefCell::new(crate::talk::app::Talk::default()),
        talk_cfg: RefCell::new(crate::config::TalkConfig::default()),
        hotkey: RefCell::new(None),
        hotkey_error: RefCell::new(None),
        talk_item: RefCell::new(None),
        talk_speak_item: RefCell::new(None),
        talk_line: RefCell::new(None),
    });
    let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };

    let menu = NSMenu::new(mtm);
    let record_call = item(mtm, "Record this call", Some(sel!(recordThisCall:)), "");
    let decline = item(mtm, "Not this one", Some(sel!(notThisOne:)), "");
    let start = item(mtm, "Start Recording", Some(sel!(startRecording:)), "r");
    let stop = item(mtm, "Stop Recording", Some(sel!(stopRecording:)), "s");
    let level = item(mtm, "", None, "");
    let queue_line = item(mtm, "", None, "");
    let assistant = item(mtm, "Live Assistant", Some(sel!(toggleAssistant:)), "");
    let assistant_line = item(mtm, "", None, "");
    let talk_item = item(
        mtm,
        &format!("Hold to talk to firstmate {}", crate::hotkey::CHORD),
        Some(sel!(talkToggle:)),
        "",
    );
    let talk_speak = item(
        mtm,
        "Speak firstmate's replies",
        Some(sel!(toggleTalkSpeak:)),
        "",
    );
    let talk_line = item(mtm, "", None, "");
    let settings = item(mtm, "Settings…", Some(sel!(openSettings:)), ",");
    let open = item(mtm, "Open Ambient", Some(sel!(openWindow:)), "0");
    let quit = item(mtm, "Quit Ambient", Some(sel!(terminate:)), "q");

    for i in [
        &start,
        &stop,
        &settings,
        &open,
        &record_call,
        &decline,
        &assistant,
        &talk_item,
        &talk_speak,
    ] {
        unsafe { i.setTarget(Some(&delegate)) };
    }
    {
        level.setEnabled(false);
        level.setHidden(true);
        queue_line.setEnabled(false);
        queue_line.setHidden(true);
        assistant_line.setEnabled(false);
        assistant_line.setHidden(true);
        talk_line.setEnabled(false);
        for i in [&talk_item, &talk_speak, &talk_line] {
            i.setHidden(true);
        }
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
        menu.addItem(&assistant);
        menu.addItem(&assistant_line);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&talk_item);
        menu.addItem(&talk_line);
        menu.addItem(&talk_speak);
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
    *delegate.ivars().assistant_item.borrow_mut() = Some(assistant);
    *delegate.ivars().assistant_line.borrow_mut() = Some(assistant_line);
    *delegate.ivars().talk_item.borrow_mut() = Some(talk_item);
    *delegate.ivars().talk_speak_item.borrow_mut() = Some(talk_speak);
    *delegate.ivars().talk_line.borrow_mut() = Some(talk_line);
    delegate.read_assistant();
    delegate.read_talk();
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
