//! Drives the real WKWebView host, dispatcher, phase events and menu actions.
//! All session writes stay under a disposable AMBIENT_HOME.
use ambient::session::{Meter, SessionDir};
use ambient::state::{Live, Phase};
use ambient::window::MainWindow;
use block2::RcBlock;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate};
use objc2_foundation::{MainThreadMarker, NSError, NSObject, NSObjectProtocol, NSString, NSTimer};
use objc2_web_kit::WKWebView;
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::time::Instant;

struct Ivars {
    stopped: Cell<bool>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AmbientWindowCheckDelegate"]
    #[ivars = Ivars]
    struct Probe;
    unsafe impl NSObjectProtocol for Probe {}
    unsafe impl NSApplicationDelegate for Probe {}
    impl Probe {
        #[unsafe(method(stopRecording:))]
        fn stop_recording(&self, _sender: Option<&NSObject>) { self.ivars().stopped.set(true); }
    }
);

fn query(
    web: &WKWebView,
    script: &str,
    label: &'static str,
    failures: Rc<RefCell<Vec<String>>>,
    replies: Rc<Cell<usize>>,
) {
    let callback = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
        let result = unsafe { value.as_ref() }
            .and_then(|v| v.downcast_ref::<NSString>())
            .map(|v| v.to_string());
        if !error.is_null() || result.as_deref() != Some("ok") {
            failures.borrow_mut().push(format!("{label}: {result:?}"));
        }
        println!(
            "{label}: {}",
            result.unwrap_or_else(|| "JavaScript error".into())
        );
        replies.set(replies.get() + 1);
    });
    unsafe {
        web.evaluateJavaScript_completionHandler(&NSString::from_str(script), Some(&callback));
    }
}

fn main() {
    let mtm = MainThreadMarker::new().unwrap();
    let root = std::env::temp_dir().join(format!("ambient-windowcheck-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("AMBIENT_HOME", &root);
    let dir = root.join("finished");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("session.json"), json!({
        "id":"finished", "name":"Host check", "started_at":"2026-09-06T12:00:00+10:00", "ended_at":"",
        "duration_s":60, "device_hz":16000, "channels":1, "mic_channels":1, "apps":[], "model":"test"
    }).to_string()).unwrap();
    std::fs::write(dir.join("raw.jsonl"), "{\"track\":\"room\",\"start_ms\":0,\"end_ms\":1000,\"text\":\"The real host reads this transcript.\",\"confidence\":0.9}\n").unwrap();
    std::fs::write(dir.join("transcript.md"), "# Host check\n").unwrap();
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    ambient::window::install_main_menu(mtm);
    let probe = Probe::alloc(mtm).set_ivars(Ivars {
        stopped: Cell::new(false),
    });
    let probe: objc2::rc::Retained<Probe> = unsafe { msg_send![super(probe), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*probe)));
    let window = MainWindow::open(mtm);
    window.show(mtm);
    let native = app
        .windows()
        .iter()
        .find(|w| w.title().to_string() == "Ambient")
        .unwrap();
    let web = native
        .contentView()
        .unwrap()
        .downcast::<WKWebView>()
        .expect("the real content view is a WKWebView");
    println!("{}", window.describe_state());
    let meter = Arc::new(Meter::default());
    meter.elapsed_ms.store(42_000, Ordering::Relaxed);
    meter.room_peak_milli.store(300, Ordering::Relaxed);
    meter.call_peak_milli.store(600, Ordering::Relaxed);
    meter.audio_arriving.store(true, Ordering::Relaxed);
    let (sender, receiver) = mpsc::channel();
    let (recording, _) = Phase::idle().start(Live {
        dir: Arc::new(SessionDir::claim(&root, None).unwrap()),
        started: Instant::now(),
        app: None,
        result: receiver,
        meter,
    });
    let failures = Rc::new(RefCell::new(Vec::new()));
    let replies = Rc::new(Cell::new(0));
    let step = Cell::new(0);
    let idle = Phase::idle();
    let block = RcBlock::new(move |_timer: core::ptr::NonNull<NSTimer>| {
        let _keep_sender = &sender;
        let n = step.get() + 1;
        step.set(n);
        window.render(
            if (9..=15).contains(&n) {
                &recording
            } else {
                &idle
            },
            None,
            mtm,
        );
        let check = |script, label| query(&web, script, label, failures.clone(), replies.clone());
        match n {
            6 => check(
                r#"(() => { const row = document.querySelector('[data-session="finished"]'); if (!row) return document.body.innerText; row.click(); return 'ok'; })()"#,
                "select session",
            ),
            8 => check(
                r#"document.querySelector('main')?.dataset.selectedSession === 'finished' && document.body.innerText.includes('The real host reads this transcript.') ? 'ok' : document.body.innerText"#,
                "real transcript",
            ),
            11 => check(
                r#"document.querySelector('[data-testid="live-clock"]')?.textContent === '00:42' && document.querySelector('[data-testid="live-meter-room"]')?.dataset.level === '0.3' && document.querySelector('main')?.dataset.selectedSession === 'finished' ? 'ok' : document.body.innerText"#,
                "phase and selection",
            ),
            12 => check(
                r#"(() => { const button = document.querySelector('[data-testid="record-stop"]'); if (!button) return 'missing Stop'; button.click(); return 'ok'; })()"#,
                "Stop through real bridge",
            ),
            14 => {
                if !probe.ivars().stopped.get() {
                    failures
                        .borrow_mut()
                        .push("Stop did not reach delegate".into());
                }
                window.select_settings();
            }
            16 => check(
                r#"document.querySelector('[data-testid="diarize-switch"]') ? 'ok' : document.body.innerText"#,
                "Settings navigation",
            ),
            17 => check(
                "(() => { const original = window.ambient.event; window.ambient.event = (name, payload) => { if (name === 'command') window.menuReached = true; original(name, payload); }; return 'ok'; })()",
                "listen for File command",
            ),
            18 => unsafe {
                NSApplication::sharedApplication(mtm).sendAction_to_from(
                    sel!(copyMarkdown:),
                    None,
                    None,
                );
            },
            20 => check(
                "window.menuReached === true ? 'ok' : 'File command missing'",
                "native File command",
            ),
            22 => {
                if replies.get() != 7 {
                    failures
                        .borrow_mut()
                        .push(format!("only {} query replies", replies.get()));
                }
                let errors = failures.borrow();
                if errors.is_empty() {
                    println!(
                        "windowcheck: ok — WKWebView, transcript, phase, selection, Stop, Settings, File menu"
                    );
                } else {
                    for error in errors.iter() {
                        eprintln!("FAILED: {error}");
                    }
                }
                std::process::exit(i32::from(!errors.is_empty()));
            }
            _ => {}
        }
    });
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.4, true, &block);
    }
    std::mem::forget(block);
    app.run();
}
