//! Drives the real session browser through the real `render(&Phase)` and
//! reports what the window actually shows.
//!
//! The same reason `uicheck` exists: compiling and a green `cargo test` prove
//! the window *says* the right thing, and only this proves it does anything.
//! Step 7's two hazards are both invisible to inspection —
//!
//! 1. pinning the live row at index 0 shifts every session's row index on the
//!    way into and out of `Recording`, and
//! 2. `reloadData` clears the selection without firing
//!    `tableViewSelectionDidChange:`
//!
//! — so the script below selects nothing by row index anywhere, and asserts
//! that a session selected before a recording starts is still the selected
//! session afterwards, at its new row.
//!
//! The meter is written here the way the capture worker writes it: the same
//! atomics, once a second. No audio device is opened, which is the point — the
//! window's whole live readout is supposed to come off shared memory rather
//! than the filesystem, and if it ever needed a real recording to draw
//! something, that would be the bug.
//!
//! Run: `AMBIENT_HOME=<scratch> cargo run --bin windowcheck`

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

use ambient::session::{self, Meter, MeterPhase, SessionDir};
use ambient::state::{Live, Phase};
use ambient::window::MainWindow;

use block2::RcBlock;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate};
use objc2_foundation::{
    MainThreadMarker, NSObject, NSObjectProtocol, NSRunLoop, NSRunLoopCommonModes, NSTimer,
};

/// Stands in for the menu bar delegate, which is the object the real Stop
/// button has to reach. The window's Stop forwards `stopRecording:` up the
/// responder chain rather than doing anything itself, and whether that chain
/// arrives anywhere is not a thing source can be read for.
struct ProbeIvars {
    stopped: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AmbientWindowCheckDelegate"]
    #[ivars = ProbeIvars]
    struct Probe;

    unsafe impl NSObjectProtocol for Probe {}
    unsafe impl NSApplicationDelegate for Probe {}

    impl Probe {
        #[unsafe(method(stopRecording:))]
        fn stop_recording(&self, _sender: Option<&NSObject>) {
            self.ivars().stopped.set(true);
        }
    }
);

/// One live recording, minus the worker. The `Sender` is kept alive so the
/// receiver never reports the thread as gone.
struct Fake {
    phase: Phase,
    meter: Arc<Meter>,
    id: String,
    #[allow(dead_code)]
    tx: Sender<anyhow::Result<PathBuf>>,
}

fn begin(home: &std::path::Path) -> Fake {
    let dir = Arc::new(SessionDir::claim(home, None).expect("claim a session directory"));
    let id = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let (tx, rx): (_, Receiver<anyhow::Result<PathBuf>>) = channel();
    let meter = Arc::new(Meter::default());
    let live = Live {
        dir,
        started: Instant::now(),
        app: Some("us.zoom.xos".into()),
        result: rx,
        meter: meter.clone(),
    };
    let (phase, orphan) = Phase::idle().start(live);
    assert!(orphan.is_none(), "an idle phase takes the recording");
    Fake {
        phase,
        meter,
        id,
        tx,
    }
}

/// What the worker thread does once a second, minus the audio.
fn write_meter(m: &Meter, elapsed_s: u64, room: f32, call: f32) {
    m.elapsed_ms.store(elapsed_s * 1000, Ordering::Relaxed);
    m.room_peak_milli
        .store((room * 1000.0) as u32, Ordering::Relaxed);
    m.call_peak_milli
        .store((call * 1000.0) as u32, Ordering::Relaxed);
    m.audio_arriving.store(room + call > 0.0, Ordering::Relaxed);
}

fn main() {
    let mtm = MainThreadMarker::new().expect("main thread");
    let home = session::home();
    std::fs::create_dir_all(&home).expect("sessions folder");
    println!("sessions folder: {}", home.display());

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    ambient::window::install_main_menu(mtm);

    let probe = Probe::alloc(mtm).set_ivars(ProbeIvars {
        stopped: Cell::new(false),
    });
    let probe: objc2::rc::Retained<Probe> = unsafe { msg_send![super(probe), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*probe)));

    let window = MainWindow::open(mtm);
    window.show(mtm);

    // Where a nil-targeted `stopRecording:` lands with this window key. The
    // window's Stop button does exactly this send; if the answer is `None`,
    // the button is a no-op that looks like a button.
    let target = unsafe { app.targetForAction(sel!(stopRecording:)) };
    println!(
        "stopRecording: reaches {}",
        target
            .as_ref()
            .map(|t| t.class().name().to_string_lossy().into_owned())
            .unwrap_or_else(|| "NOBODY".into())
    );

    let phase = RefCell::new(Phase::idle());
    let live = RefCell::new(None::<Fake>);
    let step = Cell::new(0u32);
    let failures = Cell::new(0u32);

    let block = RcBlock::new(move |_t: core::ptr::NonNull<NSTimer>| {
        let n = step.get();
        step.set(n + 1);

        // The script. Everything the window is told arrives through
        // `render(&Phase)` and nothing else.
        match n {
            0 => {}
            1 => {
                let f = begin(&home);
                println!("\n-- recording A started ({}) --", f.id);
                *phase.borrow_mut() = Phase::idle();
                write_meter(&f.meter, 0, 0.0, 0.0);
                *live.borrow_mut() = Some(f);
            }
            2..=5 => {
                if let Some(f) = live.borrow().as_ref() {
                    // A climbing room level and a falling call level: an
                    // indicator wired to the cumulative peaks instead of the
                    // per-interval ones would never show the fall.
                    let s = (n - 1) as u64;
                    write_meter(&f.meter, s, 0.1 * s as f32, 0.9 - 0.2 * s as f32);
                }
            }
            6 => {
                println!("\n-- the microphone goes silent --");
                if let Some(f) = live.borrow().as_ref() {
                    write_meter(&f.meter, 5, 0.0, 0.0);
                }
            }
            7 => {
                println!("\n-- Stop pressed: the worker moves on --");
                let mut slot = live.borrow_mut();
                let Fake {
                    phase: p,
                    meter,
                    id,
                    tx,
                } = slot.take().expect("a recording is running");
                let p = p.stop().unwrap_or_else(|(_, e)| panic!("stop failed: {e}"));
                meter
                    .phase
                    .store(MeterPhase::Transcribing as u8, Ordering::Relaxed);
                *slot = Some(Fake {
                    phase: p,
                    meter,
                    id,
                    tx,
                });
            }
            8 => {
                println!("\n-- recording A finished --");
                let f = live.borrow_mut().take().expect("a recording is running");
                f.meter
                    .phase
                    .store(MeterPhase::Done as u8, Ordering::Relaxed);
                *phase.borrow_mut() = f.phase.finished(Ok(PathBuf::from("/dev/null")));
            }
            9 => {
                let f = begin(&home);
                println!(
                    "\n-- recording B started ({}) — A must keep the selection --",
                    f.id
                );
                write_meter(&f.meter, 0, 0.4, 0.4);
                *live.borrow_mut() = Some(f);
            }
            // Nothing changes: the row set is stable, so the table has laid
            // its rows out and the pinned row can be read back. On the turn a
            // `reloadData` happens there is no cell view to ask yet, which is
            // why the check below is made here and not on every tick.
            10 => {}
            _ => {
                println!(
                    "\nwindowcheck: {}",
                    if failures.get() == 0 {
                        "ok".to_string()
                    } else {
                        format!("{} FAILURE(S)", failures.get())
                    }
                );
                NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None);
                return;
            }
        }

        // One render per turn, from whichever phase is current — exactly the
        // call the delegate's timer makes.
        let borrowed = live.borrow();
        match borrowed.as_ref() {
            Some(f) => window.render(&f.phase, None, mtm),
            None => window.render(&phase.borrow(), None, mtm),
        }
        let state = window.describe_state();
        println!("{n}: {state}");

        // The two properties that cannot be read off the source.
        let expect = |cond: bool, what: &str| {
            if !cond {
                failures.set(failures.get() + 1);
                println!("   FAILED: {what}");
            }
        };
        if let Some(f) = borrowed.as_ref() {
            expect(
                state.contains(&format!("live: {}", f.id)),
                "the live recording is the pinned row",
            );
            if matches!(n, 2..=6 | 10) {
                expect(
                    state.contains("row 0: \"● "),
                    "row 0 is drawn as the live row",
                );
            }
            if matches!(n, 2..=5) {
                expect(
                    state.contains("[Stop Recording]"),
                    "Stop is offered while the phase is Recording",
                );
            }
            if n == 7 {
                // The phase governs whether Stop is legal; the worker's own
                // meter governs what the button is allowed to claim.
                expect(
                    state.contains("[Transcribing…, disabled]"),
                    "Stop stands down once the phase has left Recording",
                );
            }
            if n == 9 {
                // The hazard this whole step carries: A was selected, B's
                // live row went in above it, and A must still be selected —
                // at its new index.
                expect(
                    !state.contains(&format!("selected: {}", f.id)),
                    "starting a recording does not steal the selection",
                );
                expect(
                    state.contains("at row 1"),
                    "the selected session moved down one row and kept the selection",
                );
            }
        }
    });
    unsafe {
        let timer = NSTimer::timerWithTimeInterval_repeats_block(0.5, true, &block);
        NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
    }
    std::mem::forget(block);
    app.run();
}
