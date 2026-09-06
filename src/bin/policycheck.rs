//! Drives the real activation-policy flip and reports what the app actually
//! becomes.
//!
//! The flip is the highest-risk piece of the design and none of it is visible
//! to inspection: whether the app has a Dock icon is a property of the running
//! process, and both guards on the demotion are about windows that only exist
//! at run time. In particular `NSApp.windows()` carries AppKit's own furniture
//! — the status item's bar window, the carrier behind an open menu — and a
//! guard that counted those would mean the app never demoted at all. This
//! prints the list rather than assuming it.
//!
//! It opens the same `MainWindow` the menu bar app opens, closes it the way
//! ⌘W does (`performClose:`, so `windowWillClose:` really runs), and asserts:
//!
//! 1. launch is Accessory,
//! 2. showing the window promotes to Regular,
//! 3. closing it demotes back to Accessory,
//! 4. a modal session in progress refuses the demotion,
//! 5. another visible window refuses it too,
//! 6. the refresh tick's retry demotes once that window goes away — without it
//!    the guards are one-shot and the app stays Regular with no window, and
//! 7. minimising is not closing and keeps Regular.
//!
//! Run: `AMBIENT_HOME=<scratch> cargo run --bin policycheck`

use std::cell::{Cell, RefCell};

use ambient::state::Phase;
use ambient::window::{demote_to_accessory, policy_name, MainWindow};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSStatusBar, NSVariableStatusItemLength, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSRunLoop,
    NSRunLoopCommonModes, NSSize, NSTimer,
};

/// Only here because `applicationShouldTerminateAfterLastWindowClosed` is one
/// of the two things step 8 has to get right, and the default is the wrong
/// answer. Returning false from the real delegate is what keeps the background
/// agent alive when the reader is closed; this stands in for it so closing the
/// window here does not end the check.
struct ProbeIvars {
    reopened: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AmbientPolicyCheckDelegate"]
    #[ivars = ProbeIvars]
    struct Probe;

    unsafe impl NSObjectProtocol for Probe {}
    unsafe impl NSApplicationDelegate for Probe {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn terminate_after_last_window(&self, _app: &NSApplication) -> bool {
            false
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, _visible: bool) -> bool {
            self.ivars().reopened.set(true);
            true
        }
    }
);

/// Everything `NSApp.windows()` holds, and the two properties the demotion
/// guard reads. This is the measurement the guard's filter was written from.
fn dump_windows(app: &NSApplication) {
    println!("   NSApp.windows():");
    for w in app.windows().iter() {
        println!(
            "     {:<28} visible={:<5} canBecomeKey={:<5} title={:?}",
            w.class().name().to_string_lossy(),
            w.isVisible(),
            w.canBecomeKeyWindow(),
            w.title().to_string(),
        );
    }
}

fn main() {
    let mtm = MainThreadMarker::new().expect("main thread");
    let home = ambient::session::home();
    std::fs::create_dir_all(&home).expect("sessions folder");
    println!("sessions folder: {}", home.display());

    let app = NSApplication::sharedApplication(mtm);
    // Launch, unchanged: LSUIElement in the bundle, Accessory here.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    ambient::window::install_main_menu(mtm);

    // The shipped app always has one, and its window is in `NSApp.windows()`
    // for the whole of the app's life. If that counted as "another window is
    // visible" the app would promote once and never come back down, so it is
    // created here rather than reasoned about.
    let status_item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(b) = status_item.button(mtm) {
        b.setTitle(ns_string!("policycheck"));
    }

    let probe = Probe::alloc(mtm).set_ivars(ProbeIvars {
        reopened: Cell::new(false),
    });
    let probe: Retained<Probe> = unsafe { msg_send![super(probe), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*probe)));

    let window = MainWindow::open(mtm);
    let phase = Phase::idle();
    let step = Cell::new(0u32);
    let failures = Cell::new(0u32);
    // A second window, to prove the "another window is visible" guard.
    let other: RefCell<Option<Retained<NSWindow>>> = RefCell::new(None);

    let expect = move |cond: bool, what: &str, failures: &Cell<u32>| {
        if cond {
            println!("   ok: {what}");
        } else {
            failures.set(failures.get() + 1);
            println!("   FAILED: {what}");
        }
    };

    let block = RcBlock::new(move |_t: core::ptr::NonNull<NSTimer>| {
        let app = NSApplication::sharedApplication(mtm);
        let n = step.get();
        step.set(n + 1);
        match n {
            0 => {
                println!("\n-- launched --  policy: {}", policy_name(mtm));
                expect(
                    policy_name(mtm) == "Accessory",
                    "the app starts with no Dock icon",
                    &failures,
                );
                dump_windows(&app);
            }
            1 => {
                println!("\n-- Open Ambient --");
                window.show(mtm);
                window.render(&phase, None, mtm);
                println!("   policy: {}", policy_name(mtm));
                expect(
                    policy_name(mtm) == "Regular",
                    "opening the window promotes to Regular",
                    &failures,
                );
                expect(
                    app.mainMenu().is_some(),
                    "a real main menu is installed under Regular",
                    &failures,
                );
                dump_windows(&app);
                println!("   {}", window.describe_state());
            }
            2 => {
                println!("\n-- Settings row --");
                window.select_settings();
                window.render(&phase, None, mtm);
                let state = window.describe_state();
                println!("   {state}");
                expect(
                    state.contains("settings pane: shown"),
                    "the settings page is a sibling view of this window",
                    &failures,
                );
                expect(
                    state.contains("selected: <settings>"),
                    "the Settings row stays selected rather than acting as a button",
                    &failures,
                );
            }
            3 => {
                // The guard that deadlocks if it is missing. `runModal` would
                // block this whole function, so the session is begun rather
                // than run — `NSApp.modalWindow` is set either way, and that
                // is what the guard reads. The real path is the settings
                // page's Choose… button, which runs `NSOpenPanel::runModal`.
                println!("\n-- demote refused under a modal session --");
                let panel = unsafe {
                    NSWindow::initWithContentRect_styleMask_backing_defer(
                        NSWindow::alloc(mtm),
                        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, 200.0)),
                        NSWindowStyleMask::Titled,
                        NSBackingStoreType::Buffered,
                        false,
                    )
                };
                panel.setTitle(ns_string!("policycheck modal"));
                let session = app.beginModalSessionForWindow(&panel);
                expect(
                    app.modalWindow().is_some(),
                    "a modal session really is in progress",
                    &failures,
                );
                demote_to_accessory(mtm, None);
                expect(
                    policy_name(mtm) == "Regular",
                    "the demotion refuses while a modal panel is up",
                    &failures,
                );
                unsafe { app.endModalSession(session) };
                panel.orderOut(None);
            }
            4 => {
                println!("\n-- demote refused while another window is visible --");
                let panel = unsafe {
                    NSWindow::initWithContentRect_styleMask_backing_defer(
                        NSWindow::alloc(mtm),
                        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, 200.0)),
                        NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                        NSBackingStoreType::Buffered,
                        false,
                    )
                };
                unsafe { panel.setReleasedWhenClosed(false) };
                panel.setTitle(ns_string!("policycheck other"));
                panel.makeKeyAndOrderFront(None);
                *other.borrow_mut() = Some(panel);
                dump_windows(&app);
                // The main window is passed as the one being closed, so the
                // only thing left that can refuse is the panel — otherwise
                // this would pass whether or not the guard looked past the
                // window it was closing.
                let main = app
                    .windows()
                    .iter()
                    .find(|w| w.title().to_string() == "Ambient")
                    .expect("the main window is in NSApp.windows()");
                // Ordered out first, exactly as `windowWillClose:` does it, so
                // the state this leaves behind is the real one: main window
                // gone, panel still up, policy stuck at Regular.
                main.orderOut(None);
                demote_to_accessory(mtm, Some(&main));
                expect(
                    policy_name(mtm) == "Regular",
                    "the demotion refuses while another window is visible",
                    &failures,
                );
            }
            5 => {
                // The guard in case 4 refused, correctly. But `windowWillClose:`
                // is the only other caller and it will not fire again — the main
                // window is already ordered out. Without a second ask the app is
                // left in Regular for ever: Dock icon and Cmd-Tab entry with no
                // window behind them. That second ask is the refresh tick, which
                // calls the demotion with `None`; this is that exact call.
                println!("\n-- retry after the blocking window went away --");
                if let Some(p) = other.borrow_mut().take() {
                    p.orderOut(None);
                }
                demote_to_accessory(mtm, None);
                expect(
                    policy_name(mtm) == "Accessory",
                    "the retry demotes once the window that blocked it is gone",
                    &failures,
                );
                // Back to Regular for the minimise case, the way reopening does
                // it. `show` reuses this window; `MainWindow::open` would build
                // a second one and the lookup by title below would be ambiguous.
                window.show(mtm);
            }
            6 => {
                // Minimising is not closing. An app that dropped out of the
                // Dock and Cmd-Tab when its window was minimised would have
                // put the window out of reach, which is the one thing
                // Accessory cannot undo.
                println!("\n-- minimise --");
                let main = app
                    .windows()
                    .iter()
                    .find(|w| w.title().to_string() == "Ambient")
                    .expect("the main window is in NSApp.windows()");
                main.miniaturize(None);
                expect(
                    policy_name(mtm) == "Regular",
                    "minimising keeps Regular",
                    &failures,
                );
                main.deminiaturize(None);
            }
            7 => {
                expect(
                    policy_name(mtm) == "Regular" && window.is_visible(),
                    "and un-minimising comes back to the same window",
                    &failures,
                );
                println!("\n-- ⌘W: performClose: on the main window --");
                // Found the way the menu finds it, not by holding a handle:
                // this is the window `performClose:` acts on when the File
                // menu's Close is chosen.
                let target = app
                    .windows()
                    .iter()
                    .find(|w| w.title().to_string() == "Ambient")
                    .expect("the main window is in NSApp.windows()");
                target.performClose(None);
            }
            8 => {
                println!("   policy: {}", policy_name(mtm));
                expect(
                    policy_name(mtm) == "Accessory",
                    "closing the window gives the Dock icon and the menu bar back",
                    &failures,
                );
                expect(
                    !window.is_visible(),
                    "the window was ordered out before the demotion",
                    &failures,
                );
                dump_windows(&app);
            }
            9 => {
                println!("\n-- reopened (the Dock icon's click) --");
                window.show(mtm);
                window.render(&phase, None, mtm);
                expect(
                    policy_name(mtm) == "Regular" && window.is_visible(),
                    "the window reopens and promotes again after a full cycle",
                    &failures,
                );
            }
            10 => {
                let target = app
                    .windows()
                    .iter()
                    .find(|w| w.title().to_string() == "Ambient")
                    .expect("the main window is in NSApp.windows()");
                target.performClose(None);
            }
            _ => {
                expect(
                    policy_name(mtm) == "Accessory",
                    "the second close demotes too",
                    &failures,
                );
                println!(
                    "\npolicycheck: {}",
                    if failures.get() == 0 {
                        "ok".to_string()
                    } else {
                        format!("{} FAILURE(S)", failures.get())
                    }
                );
                app.terminate(None);
            }
        }
    });
    unsafe {
        let timer = NSTimer::timerWithTimeInterval_repeats_block(0.6, true, &block);
        NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
    }
    std::mem::forget(block);
    app.run();
}
