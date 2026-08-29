//! The menu bar app.
//!
//! `NSApplication` owns the main thread, so capture runs on a worker — verified
//! rather than assumed, because a tap that is denied returns silence instead of
//! an error. A tap started on a spawned thread was measured at 0.657 on the call
//! channel, against 0.000 for the launch path that genuinely lacks the grant.
//!
//! Nothing here reaches into the capture layer. The worker runs the same
//! blocking `session::record` the CLI runs, and the two halves talk through the
//! `STOP` sentinel and `status` file that already existed for the detached
//! bundle launch — which turn out to be exactly the interface a GUI wants.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSImage, NSMenu,
    NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSObject, NSObjectProtocol, NSRunLoop, NSRunLoopCommonModes, NSString,
    NSTimer, NSURL,
};

/// What the status item is doing, which is also what its icon says.
#[derive(Clone, Copy, PartialEq)]
enum State {
    Idle,
    /// A watched app started producing audio and Ambient is waiting to be told
    /// whether to record it.
    Armed,
    Recording,
    Transcribing,
}

impl State {
    /// SF Symbols standing in for the artboard's three icons: outline when
    /// present but recording nothing, filled while live, and a distinct shape
    /// while the models are running.
    fn symbol(self) -> &'static str {
        match self {
            State::Idle => "waveform",
            State::Armed => "waveform.badge.exclamationmark",
            State::Recording => "waveform.circle.fill",
            State::Transcribing => "hourglass",
        }
    }
}

/// What the watcher should do about what is currently playing.
#[derive(Debug, PartialEq)]
enum Watch {
    Nothing,
    /// Stand down from Armed: whatever was playing has stopped.
    Disarm,
    /// Nothing is playing, so a previous "not this one" has served its purpose.
    Forget,
    Arm(String),
    Start(String),
}

/// The consent rule, kept pure so it can be tested without a menu bar.
///
/// An empty `apps` watches nothing. That list already means "capture all system
/// audio", and arming on any sound would flap at every notification chime — so
/// the Start item stays the only way in when nothing is named.
fn decide(
    state: State,
    apps: &[String],
    live: &[String],
    declined: Option<&str>,
    ask: bool,
) -> Watch {
    if apps.is_empty() {
        return Watch::Nothing;
    }
    let playing: Vec<&String> = apps
        .iter()
        .filter(|a| live.iter().any(|l| l == *a))
        .collect();
    if playing.is_empty() {
        // Nothing watched is playing. Stand down if we were waiting on an
        // answer, and forget the decline so the next call is asked afresh.
        return if state == State::Armed {
            Watch::Disarm
        } else if declined.is_some() {
            Watch::Forget
        } else {
            Watch::Nothing
        };
    }
    // A "not this one" only answers for as long as that call is still going.
    // Once its app falls quiet the decline is spent, even if something else is
    // still playing — so it stops counting here rather than a tick later.
    let declined = declined.filter(|d| playing.iter().any(|a| a.as_str() == *d));
    // Skip past anything still declined rather than stopping at it: a watched
    // app left playing all day would otherwise mask every real call behind it.
    let Some(app) = playing.iter().find(|a| declined != Some(a.as_str())) else {
        return Watch::Nothing;
    };
    if state != State::Idle {
        return Watch::Nothing;
    }
    if ask {
        Watch::Arm((*app).clone())
    } else {
        Watch::Start((*app).clone())
    }
}

struct Ivars {
    status_item: Retained<NSStatusItem>,
    state: RefCell<State>,
    /// Present only while a recording thread is alive.
    result: RefCell<Option<Receiver<anyhow::Result<PathBuf>>>>,
    quitting: RefCell<bool>,
    log: PathBuf,
    /// Held across opens so the window and its bridge survive being closed.
    settings: RefCell<Option<crate::settings::SettingsWindow>>,
    start_item: RefCell<Option<Retained<NSMenuItem>>>,
    stop_item: RefCell<Option<Retained<NSMenuItem>>>,
    level_item: RefCell<Option<Retained<NSMenuItem>>>,
    record_call_item: RefCell<Option<Retained<NSMenuItem>>>,
    decline_item: RefCell<Option<Retained<NSMenuItem>>>,
    /// The refresh timer runs at 2 Hz for the level meter; watching for calls
    /// needs nothing like that rate, so it happens every eighth tick.
    ticks: Cell<u64>,
    /// The bundle that armed us, and one the user has waved away. The decline
    /// is forgotten as soon as that app stops producing audio, so saying no to
    /// one call does not opt out of the next.
    armed_by: RefCell<Option<String>>,
    declined: RefCell<Option<String>>,
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
        /// ever. Stop it properly and let the transcription finish.
        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _app: &NSApplication) -> usize {
            let state = *self.ivars().state.borrow();
            match state {
                // Nothing is running. Armed is only ever waiting on an answer,
                // and deferring for a worker that was never spawned would hang
                // the quit for ever — the reply only goes out when a recording
                // thread hands back a result.
                State::Idle | State::Armed => 1, // NSTerminateNow
                State::Recording | State::Transcribing => {
                    if state == State::Recording {
                        crate::session::stop_recording(None).ok();
                        // Through set_state, so the icon and menu stop claiming
                        // a recording is still running.
                        self.set_state(State::Transcribing);
                    }
                    self.log("quit requested — finishing the recording first");
                    // NSTerminateLater: `refresh` replies once the worker is done.
                    *self.ivars().quitting.borrow_mut() = true;
                    2
                }
            }
        }
    }

    impl Delegate {
        #[unsafe(method(startRecording:))]
        fn start_recording(&self, _sender: Option<&AnyObject>) {
            if matches!(*self.ivars().state.borrow(), State::Recording | State::Transcribing) {
                return;
            }
            self.log("start recording");
            self.begin_recording();
        }

        /// The armed state's yes. Identical to pressing Start, but logged
        /// differently because consent given to a specific call is worth being
        /// able to point at afterwards.
        #[unsafe(method(recordThisCall:))]
        fn record_this_call(&self, _sender: Option<&AnyObject>) {
            if *self.ivars().state.borrow() != State::Armed {
                return;
            }
            let who = self.ivars().armed_by.borrow().clone().unwrap_or_default();
            self.log(&format!("recording {who} — allowed by the user"));
            self.begin_recording();
        }

        /// The armed state's no. Remembered against the bundle so the menu does
        /// not ask again two seconds later, and forgotten once that app goes
        /// quiet.
        #[unsafe(method(notThisOne:))]
        fn not_this_one(&self, _sender: Option<&AnyObject>) {
            if *self.ivars().state.borrow() != State::Armed {
                return;
            }
            let who = self.ivars().armed_by.borrow().clone();
            self.log(&format!(
                "declined {} — not recording",
                who.clone().unwrap_or_default()
            ));
            *self.ivars().declined.borrow_mut() = who;
            *self.ivars().armed_by.borrow_mut() = None;
            self.set_state(State::Idle);
        }

        #[unsafe(method(stopRecording:))]
        fn stop_recording(&self, _sender: Option<&AnyObject>) {
            if *self.ivars().state.borrow() != State::Recording {
                return;
            }
            self.log("stop requested");
            // Stopping is an answer about this call. Without recording it, the
            // watcher sees the app still playing on its next tick and starts a
            // fresh recording — leaving no way to stop until the call ends.
            let cfg = crate::config::Config::load();
            let live = crate::probe::bundles_rendering_output();
            let playing = cfg
                .apps
                .iter()
                .find(|a| live.iter().any(|l| l == *a))
                .cloned();
            let armed = self.ivars().armed_by.borrow().clone();
            *self.ivars().declined.borrow_mut() = playing.or(armed);
            // Writes the sentinel the capture loop polls every 200 ms.
            if let Err(e) = crate::session::stop_recording(None) {
                eprintln!("stop: {e}");
            }
            self.set_state(State::Transcribing);
        }

        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            let mtm = MainThreadMarker::from(self);
            let mut slot = self.ivars().settings.borrow_mut();
            let w = slot.get_or_insert_with(|| crate::settings::SettingsWindow::open(mtm));
            // The CLI can have changed the file since this window last drew.
            w.refresh();
            w.show(mtm);
        }

        #[unsafe(method(openSessions:))]
        fn open_sessions(&self, _sender: Option<&AnyObject>) {
            let home = crate::session::home();
            std::fs::create_dir_all(&home).ok();
            let s = NSString::from_str(&home.to_string_lossy());
            {
                if let Some(url) = NSURL::fileURLWithPath(&s).into() {
                    let url: Retained<NSURL> = url;
                    NSWorkspace::sharedWorkspace().openURL(&url);
                }
            }
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
        std::fs::create_dir_all(crate::session::home()).ok();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.ivars().log)
        {
            use std::io::Write;
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// Shared by the Start item and the armed state's yes.
    fn begin_recording(&self) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            // The same blocking function the CLI runs. ProcessTap is created
            // and dropped on this thread and never moves.
            tx.send(crate::session::record(None, &[], None, None)).ok();
        });
        *self.ivars().result.borrow_mut() = Some(rx);
        // armed_by is deliberately kept: stopping needs to know which call this
        // recording belongs to.
        self.set_state(State::Recording);
    }

    /// Notice a watched app starting to produce audio, and act on `decide`.
    fn poll_for_calls(&self) {
        let cfg = crate::config::Config::load();
        let live = crate::probe::bundles_rendering_output();
        // The borrow is released before anything below can borrow again.
        let declined = self.ivars().declined.borrow().clone();
        let what = decide(
            *self.ivars().state.borrow(),
            &cfg.apps,
            &live,
            declined.as_deref(),
            cfg.ask_before_recording,
        );
        match what {
            Watch::Nothing => {}
            Watch::Forget => *self.ivars().declined.borrow_mut() = None,
            Watch::Disarm => {
                *self.ivars().declined.borrow_mut() = None;
                *self.ivars().armed_by.borrow_mut() = None;
                self.set_state(State::Idle);
            }
            Watch::Arm(app) => {
                self.log(&format!("{app} is producing audio — waiting to be told"));
                // Any earlier decline is spent: `decide` only offers an app it
                // is not currently answering for.
                *self.ivars().declined.borrow_mut() = None;
                *self.ivars().armed_by.borrow_mut() = Some(app);
                self.set_state(State::Armed);
            }
            Watch::Start(app) => {
                self.log(&format!("{app} is producing audio — recording"));
                *self.ivars().declined.borrow_mut() = None;
                *self.ivars().armed_by.borrow_mut() = Some(app);
                self.begin_recording();
            }
        }
    }

    fn set_state(&self, state: State) {
        *self.ivars().state.borrow_mut() = state;
        let mtm = MainThreadMarker::from(self);
        {
            if let Some(button) = self.ivars().status_item.button(mtm) {
                let name = NSString::from_str(state.symbol());
                let desc = NSString::from_str("Ambient");
                if let Some(img) =
                    NSImage::imageWithSystemSymbolName_accessibilityDescription(&name, Some(&desc))
                {
                    img.setTemplate(true);
                    button.setImage(Some(&img));
                }
            }
        }
        let recording = state == State::Recording;
        let idle = state == State::Idle;
        let armed = state == State::Armed;
        if let Some(i) = self.ivars().start_item.borrow().as_ref() {
            i.setEnabled(idle || armed);
        }
        if let Some(i) = self.ivars().stop_item.borrow().as_ref() {
            i.setEnabled(recording);
        }
        if let Some(i) = self.ivars().level_item.borrow().as_ref() {
            i.setHidden(idle || armed);
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
                self.ivars().armed_by.borrow().as_ref(),
            ) {
                i.setTitle(&NSString::from_str(&format!("Record {app}")));
            }
        }
    }

    /// Called on a timer: mirror the recording's own status line into the menu,
    /// and notice when the worker has finished.
    fn refresh(&self) {
        let state = *self.ivars().state.borrow();

        // Every eighth tick, so roughly every four seconds. Enumerating audio
        // processes twice a second would be pure waste for something that
        // changes when a human joins a call.
        let n = self.ivars().ticks.get().wrapping_add(1);
        self.ivars().ticks.set(n);
        if n % 8 == 0 && matches!(state, State::Idle | State::Armed) {
            self.poll_for_calls();
        }

        if !matches!(state, State::Idle | State::Armed) {
            let text = crate::session::live_session()
                .or_else(|| {
                    // During transcription the scratch wavs are gone, so fall
                    // back to whichever session was written most recently.
                    let mut all: Vec<PathBuf> = std::fs::read_dir(crate::session::home())
                        .ok()?
                        .filter_map(|e| e.ok().map(|e| e.path()))
                        .collect();
                    all.sort();
                    all.pop()
                })
                .and_then(|d| std::fs::read_to_string(d.join(crate::session::STATUS_FILE)).ok())
                .unwrap_or_else(|| "starting…".into());
            if let Some(i) = self.ivars().level_item.borrow().as_ref() {
                i.setTitle(&NSString::from_str(text.trim()));
            }
        }

        let finished = {
            let guard = self.ivars().result.borrow();
            match guard.as_ref() {
                Some(rx) => match rx.try_recv() {
                    Ok(r) => Some(r),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        Some(Err(anyhow::anyhow!("the recording thread went away")))
                    }
                },
                None => None,
            }
        };
        if let Some(result) = finished {
            *self.ivars().result.borrow_mut() = None;
            match result {
                Ok(dir) => self.log(&format!("session written: {}", dir.display())),
                Err(e) => self.log(&format!("recording FAILED: {e}")),
            }
            self.set_state(State::Idle);
            if *self.ivars().quitting.borrow() {
                let mtm = MainThreadMarker::from(self);
                NSApplication::sharedApplication(mtm).replyToApplicationShouldTerminate(true);
            }
        }
    }
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

    let status_item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);

    let delegate = Delegate::alloc(mtm).set_ivars(Ivars {
        status_item: status_item.clone(),
        state: RefCell::new(State::Idle),
        result: RefCell::new(None),
        quitting: RefCell::new(false),
        // Launched from Finder there is nowhere for stderr to go, so keep our
        // own log next to the sessions — the last failure was invisible for
        // exactly this reason.
        log: crate::session::home().join("app.log"),
        settings: RefCell::new(None),
        start_item: RefCell::new(None),
        stop_item: RefCell::new(None),
        level_item: RefCell::new(None),
        record_call_item: RefCell::new(None),
        decline_item: RefCell::new(None),
        ticks: Cell::new(0),
        armed_by: RefCell::new(None),
        declined: RefCell::new(None),
    });
    let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };

    let menu = NSMenu::new(mtm);
    let record_call = item(mtm, "Record this call", Some(sel!(recordThisCall:)), "");
    let decline = item(mtm, "Not this one", Some(sel!(notThisOne:)), "");
    let start = item(mtm, "Start Recording", Some(sel!(startRecording:)), "r");
    let stop = item(mtm, "Stop Recording", Some(sel!(stopRecording:)), "s");
    let level = item(mtm, "", None, "");
    let settings = item(mtm, "Settings…", Some(sel!(openSettings:)), ",");
    let sessions = item(mtm, "Open Sessions Folder", Some(sel!(openSessions:)), "");
    let quit = item(mtm, "Quit Ambient", Some(sel!(terminate:)), "q");

    for i in [&start, &stop, &settings, &sessions, &record_call, &decline] {
        unsafe { i.setTarget(Some(&delegate)) };
    }
    {
        level.setEnabled(false);
        level.setHidden(true);
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
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&settings);
        menu.addItem(&sessions);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&quit);
        status_item.setMenu(Some(&menu));
    }

    *delegate.ivars().start_item.borrow_mut() = Some(start);
    *delegate.ivars().stop_item.borrow_mut() = Some(stop);
    *delegate.ivars().level_item.borrow_mut() = Some(level);
    *delegate.ivars().record_call_item.borrow_mut() = Some(record_call);
    *delegate.ivars().decline_item.borrow_mut() = Some(decline);
    delegate.set_state(State::Idle);
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

    fn apps() -> Vec<String> {
        vec!["us.zoom.xos".into(), "com.microsoft.teams2".into()]
    }

    /// The rule that matters most: with nothing named, nothing is ever armed.
    /// An empty list means "all system audio", so arming on it would ask about
    /// every notification chime.
    #[test]
    fn an_empty_watch_list_never_arms() {
        let live = vec!["us.zoom.xos".to_string()];
        assert_eq!(decide(State::Idle, &[], &live, None, true), Watch::Nothing);
    }

    #[test]
    fn a_watched_app_playing_arms_when_asked_to_ask() {
        let live = vec!["us.zoom.xos".to_string()];
        assert_eq!(
            decide(State::Idle, &apps(), &live, None, true),
            Watch::Arm("us.zoom.xos".into())
        );
    }

    #[test]
    fn with_asking_off_it_records_by_itself() {
        let live = vec!["com.microsoft.teams2".to_string()];
        assert_eq!(
            decide(State::Idle, &apps(), &live, None, false),
            Watch::Start("com.microsoft.teams2".into())
        );
    }

    #[test]
    fn an_unwatched_app_playing_is_ignored() {
        let live = vec!["com.spotify.client".to_string()];
        assert_eq!(
            decide(State::Idle, &apps(), &live, None, true),
            Watch::Nothing
        );
    }

    /// Declining must actually stick, or the menu asks again four seconds later
    /// and the answer means nothing.
    #[test]
    fn a_declined_call_is_not_asked_about_again() {
        let live = vec!["us.zoom.xos".to_string()];
        assert_eq!(
            decide(State::Idle, &apps(), &live, Some("us.zoom.xos"), true),
            Watch::Nothing
        );
    }

    /// ...but declining one call must not opt out of the next one.
    #[test]
    fn the_decline_is_forgotten_once_the_call_ends() {
        assert_eq!(
            decide(State::Idle, &apps(), &[], Some("us.zoom.xos"), true),
            Watch::Forget
        );
    }

    #[test]
    fn declining_one_app_does_not_silence_another() {
        let live = vec!["com.microsoft.teams2".to_string()];
        assert_eq!(
            decide(State::Idle, &apps(), &live, Some("us.zoom.xos"), true),
            Watch::Arm("com.microsoft.teams2".into())
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
            decide(State::Idle, &apps(), &live, Some("us.zoom.xos"), true),
            Watch::Arm("com.microsoft.teams2".into())
        );
    }

    #[test]
    fn everything_playing_being_declined_asks_nothing() {
        let live = vec!["us.zoom.xos".to_string()];
        assert_eq!(
            decide(State::Idle, &apps(), &live, Some("us.zoom.xos"), true),
            Watch::Nothing
        );
    }

    #[test]
    fn a_call_ending_while_armed_stands_down() {
        assert_eq!(
            decide(State::Armed, &apps(), &[], None, true),
            Watch::Disarm
        );
    }

    /// A recording in progress must never be disturbed by the watcher.
    #[test]
    fn recording_and_transcribing_are_left_alone() {
        let live = vec!["us.zoom.xos".to_string()];
        for state in [State::Recording, State::Transcribing, State::Armed] {
            assert_eq!(decide(state, &apps(), &live, None, true), Watch::Nothing);
        }
    }
}
