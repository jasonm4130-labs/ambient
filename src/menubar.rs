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

use std::cell::RefCell;
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
    MainThreadMarker, NSObject, NSObjectProtocol, NSString, NSTimer, NSURL,
};

/// What the status item is doing, which is also what its icon says.
#[derive(Clone, Copy, PartialEq)]
enum State {
    Idle,
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
            State::Recording => "waveform.circle.fill",
            State::Transcribing => "hourglass",
        }
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
            if *self.ivars().state.borrow() == State::Idle {
                return 1; // NSTerminateNow
            }
            if *self.ivars().state.borrow() == State::Recording {
                crate::session::stop_recording(None).ok();
                *self.ivars().state.borrow_mut() = State::Transcribing;
            }
            self.log("quit requested — finishing the recording first");
            // NSTerminateLater: `refresh` calls replyToApplicationShouldTerminate
            // once the worker hands back a result.
            *self.ivars().quitting.borrow_mut() = true;
            2
        }
    }

    impl Delegate {
        #[unsafe(method(startRecording:))]
        fn start_recording(&self, _sender: Option<&AnyObject>) {
            if *self.ivars().state.borrow() != State::Idle {
                return;
            }
            self.log("start recording");
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                // The same blocking function the CLI runs. ProcessTap is
                // created and dropped on this thread and never moves.
                tx.send(crate::session::record(None, &[], None, None)).ok();
            });
            *self.ivars().result.borrow_mut() = Some(rx);
            self.set_state(State::Recording);
        }

        #[unsafe(method(stopRecording:))]
        fn stop_recording(&self, _sender: Option<&AnyObject>) {
            if *self.ivars().state.borrow() != State::Recording {
                return;
            }
            self.log("stop requested");
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
        if let Some(i) = self.ivars().start_item.borrow().as_ref() {
            i.setEnabled(idle);
        }
        if let Some(i) = self.ivars().stop_item.borrow().as_ref() {
            i.setEnabled(recording);
        }
        if let Some(i) = self.ivars().level_item.borrow().as_ref() {
            i.setHidden(idle);
        }
    }

    /// Called on a timer: mirror the recording's own status line into the menu,
    /// and notice when the worker has finished.
    fn refresh(&self) {
        let state = *self.ivars().state.borrow();

        if state != State::Idle {
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
    });
    let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };

    let menu = NSMenu::new(mtm);
    let start = item(mtm, "Start Recording", Some(sel!(startRecording:)), "r");
    let stop = item(mtm, "Stop Recording", Some(sel!(stopRecording:)), "s");
    let level = item(mtm, "", None, "");
    let settings = item(mtm, "Settings…", Some(sel!(openSettings:)), ",");
    let sessions = item(mtm, "Open Sessions Folder", Some(sel!(openSessions:)), "");
    let quit = item(mtm, "Quit Ambient", Some(sel!(terminate:)), "q");

    for i in [&start, &stop, &settings, &sessions] {
        unsafe { i.setTarget(Some(&delegate)) };
    }
    {
        level.setEnabled(false);
        level.setHidden(true);
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
    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.5, true, &block) };
    std::mem::forget(block);

    let object = ProtocolObject::from_ref(&*delegate);
    app.setDelegate(Some(object));
    std::mem::forget(delegate);
    app.run();
    Ok(())
}
