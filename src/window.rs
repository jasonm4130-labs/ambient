//! One native window hosting the self-contained React page.
//! AppKit owns activation and menu dispatch; the page owns session presentation.

use crate::settings::SettingsPane;
use crate::state::{Phase, PhaseKind};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEventModifierFlags, NSMenu,
    NSMenuItem, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSSize, NSString,
};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

struct Ivars {
    page: SettingsPane,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AmbientWindowHost"]
    #[ivars = Ivars]
    struct Host;
    unsafe impl NSObjectProtocol for Host {}
    unsafe impl NSWindowDelegate for Host {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, notification: &NSNotification) {
            let window = notification.object().and_then(|object| object.downcast::<NSWindow>().ok());
            if let Some(window) = &window { window.orderOut(None); }
            demote_to_accessory(MainThreadMarker::from(self), window.as_deref());
        }
    }
    impl Host {
        #[unsafe(method(copyMarkdown:))]
        fn copy_markdown(&self, _sender: Option<&AnyObject>) {
            self.ivars().page.event("command", &json!({"action": "copy-markdown"}));
        }
        #[unsafe(method(revealInFinder:))]
        fn reveal_in_finder(&self, _sender: Option<&AnyObject>) {
            self.ivars().page.event("command", &json!({"action": "reveal"}));
        }
    }
);

pub struct MainWindow {
    window: Retained<NSWindow>,
    host: Retained<Host>,
}

impl MainWindow {
    pub fn open(mtm: MainThreadMarker) -> Self {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(960.0, 620.0));
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(ns_string!("Ambient"));
        unsafe {
            window.setReleasedWhenClosed(false);
            window.setContentMinSize(NSSize::new(700.0, 440.0));
        }
        window.setFrameAutosaveName(ns_string!("AmbientMain"));
        let page = SettingsPane::new(mtm);
        page.view().setHidden(false);
        window.setContentView(Some(page.view()));
        window.center();
        let host = Host::alloc(mtm).set_ivars(Ivars { page });
        let host: Retained<Host> = unsafe { msg_send![super(host), init] };
        window.setDelegate(Some(ProtocolObject::from_ref(&*host)));
        Self { window, host }
    }

    pub fn render(&self, phase: &Phase, queue: Option<&str>, _mtm: MainThreadMarker) {
        let page = &self.host.ivars().page;
        page.poll_diarize();
        page.set_recording(phase.live().is_some());
        if self.window.isVisible() {
            page.event("phase", &phase_payload(phase, queue));
        }
    }

    pub fn show(&self, mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        self.window.makeKeyAndOrderFront(None);
        self.host.ivars().page.select_sessions();
        self.host.ivars().page.refresh();
    }

    pub fn select_settings(&self) {
        self.host.ivars().page.select_settings();
        self.host.ivars().page.refresh();
    }

    pub fn is_visible(&self) -> bool {
        self.window.isVisible()
    }

    pub fn describe_state(&self) -> String {
        format!(
            "window visible: {}, policy: {}, content: WKWebView, {}",
            self.window.isVisible(),
            policy_name(MainThreadMarker::from(&*self.host)),
            self.host.ivars().page.describe()
        )
    }
}

fn phase_payload(phase: &Phase, queue: Option<&str>) -> Value {
    let kind = match phase.kind() {
        PhaseKind::Idle => "idle",
        PhaseKind::Armed => "armed",
        PhaseKind::Recording => "recording",
        PhaseKind::Stopping => "stopping",
        PhaseKind::Failed => "failed",
    };
    let live = phase.live().map(|live| {
        let m = &live.meter;
        let id = live
            .dir
            .path()
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let room = (m.room_peak_milli.load(Ordering::Relaxed) as f64 / 1000.0).clamp(0.0, 1.0);
        let call = (m.call_peak_milli.load(Ordering::Relaxed) as f64 / 1000.0).clamp(0.0, 1.0);
        json!({
            "id": id,
            "elapsed_s": m.elapsed_ms.load(Ordering::Relaxed) / 1000,
            "room_level": room,
            "call_level": call,
            "audio_arriving": m.audio_arriving.load(Ordering::Relaxed),
            "status_line": m.status_line(),
        })
    });
    let failure = phase.failure().map(|error| {
        let session = phase
            .failed_dir()
            .and_then(|d| d.file_name())
            .map(|s| s.to_string_lossy().into_owned());
        json!({"error": error, "session": session})
    });
    json!({
        "kind": kind,
        "app": phase.app(),
        "live": live,
        "queue": queue,
        "failure": failure,
    })
}

/// `NSApp.windows()` includes AppKit furniture (the status item's bar window,
/// the carrier behind an open `NSMenu`), all visible; counting them would block
/// demotion. They refuse key, so `canBecomeKeyWindow` filters them out.
fn other_visible_window(
    app: &NSApplication,
    except: Option<&NSWindow>,
) -> Option<Retained<NSWindow>> {
    app.windows().iter().find(|w| {
        let same = except.is_some_and(|e| std::ptr::eq(&**w as *const NSWindow, e));
        !same && w.isVisible() && w.canBecomeKeyWindow()
    })
}

/// Back to Accessory, unless another window is up (demoting strips its main
/// menu) or a modal session is running (changing the activation policy under
/// `runModal` deadlocks).
pub fn demote_to_accessory(mtm: MainThreadMarker, closing: Option<&NSWindow>) {
    let app = NSApplication::sharedApplication(mtm);
    if app.activationPolicy() != NSApplicationActivationPolicy::Regular {
        return;
    }
    // Log a refusal only from `windowWillClose:`; the retry tick passes `None`
    // twice a second.
    let announce = closing.is_some();
    if app.modalWindow().is_some() {
        if announce {
            eprintln!("window: staying Regular — a modal panel is up");
        }
        return;
    }
    if let Some(w) = other_visible_window(&app, closing) {
        if announce {
            eprintln!("window: staying Regular — {} is still visible", w.title());
        }
        return;
    }
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    eprintln!("window: closed — back to Accessory");
}

pub fn policy_name(mtm: MainThreadMarker) -> &'static str {
    match NSApplication::sharedApplication(mtm).activationPolicy() {
        NSApplicationActivationPolicy::Regular => "Regular",
        NSApplicationActivationPolicy::Accessory => "Accessory",
        _ => "Prohibited",
    }
}

fn menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<objc2::runtime::Sel>,
    key: &str,
    mask: Option<NSEventModifierFlags>,
) -> Retained<NSMenuItem> {
    let i = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    };
    if let Some(m) = mask {
        i.setKeyEquivalentModifierMask(m);
    }
    i
}

fn submenu(
    mtm: MainThreadMarker,
    title: &str,
    items: Vec<Retained<NSMenuItem>>,
) -> Retained<NSMenu> {
    let m = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    for i in items {
        m.addItem(&i);
    }
    m
}

/// Items are nil-targeted so they dispatch down the responder chain. Required
/// under Regular: without it the menu bar holds only the Apple menu.
pub fn install_main_menu(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let cmd_shift = NSEventModifierFlags::Command | NSEventModifierFlags::Shift;
    let cmd_opt = NSEventModifierFlags::Command | NSEventModifierFlags::Option;

    let ambient = submenu(
        mtm,
        "Ambient",
        vec![
            menu_item(
                mtm,
                "About Ambient",
                Some(sel!(orderFrontStandardAboutPanel:)),
                "",
                None,
            ),
            NSMenuItem::separatorItem(mtm),
            menu_item(mtm, "Settings…", Some(sel!(openSettings:)), ",", None),
            NSMenuItem::separatorItem(mtm),
            menu_item(mtm, "Hide Ambient", Some(sel!(hide:)), "h", None),
            menu_item(
                mtm,
                "Hide Others",
                Some(sel!(hideOtherApplications:)),
                "h",
                Some(cmd_opt),
            ),
            menu_item(
                mtm,
                "Show All",
                Some(sel!(unhideAllApplications:)),
                "",
                None,
            ),
            NSMenuItem::separatorItem(mtm),
            menu_item(mtm, "Quit Ambient", Some(sel!(terminate:)), "q", None),
        ],
    );

    let edit = submenu(
        mtm,
        "Edit",
        vec![
            menu_item(mtm, "Undo", Some(sel!(undo:)), "z", None),
            menu_item(mtm, "Redo", Some(sel!(redo:)), "z", Some(cmd_shift)),
            NSMenuItem::separatorItem(mtm),
            menu_item(mtm, "Cut", Some(sel!(cut:)), "x", None),
            menu_item(mtm, "Copy", Some(sel!(copy:)), "c", None),
            menu_item(mtm, "Paste", Some(sel!(paste:)), "v", None),
            NSMenuItem::separatorItem(mtm),
            menu_item(mtm, "Select All", Some(sel!(selectAll:)), "a", None),
        ],
    );

    let file = submenu(
        mtm,
        "File",
        vec![
            menu_item(
                mtm,
                "Start Recording",
                Some(sel!(startRecording:)),
                "r",
                None,
            ),
            menu_item(mtm, "Stop Recording", Some(sel!(stopRecording:)), "s", None),
            NSMenuItem::separatorItem(mtm),
            menu_item(
                mtm,
                "Copy Markdown",
                Some(sel!(copyMarkdown:)),
                "c",
                Some(cmd_shift),
            ),
            menu_item(
                mtm,
                "Reveal in Finder",
                Some(sel!(revealInFinder:)),
                "r",
                Some(cmd_shift),
            ),
            NSMenuItem::separatorItem(mtm),
            menu_item(mtm, "Close", Some(sel!(performClose:)), "w", None),
        ],
    );

    let windows = submenu(
        mtm,
        "Window",
        vec![
            menu_item(mtm, "Open Ambient", Some(sel!(openWindow:)), "0", None),
            menu_item(mtm, "Minimize", Some(sel!(performMiniaturize:)), "m", None),
            menu_item(mtm, "Zoom", Some(sel!(performZoom:)), "", None),
            NSMenuItem::separatorItem(mtm),
            menu_item(
                mtm,
                "Bring All to Front",
                Some(sel!(arrangeInFront:)),
                "",
                None,
            ),
        ],
    );

    let main = NSMenu::new(mtm);
    for m in [&ambient, &edit, &file, &windows] {
        let holder = menu_item(mtm, &m.title().to_string(), None, "", None);
        holder.setSubmenu(Some(m));
        main.addItem(&holder);
    }
    app.setMainMenu(Some(&main));
    app.setWindowsMenu(Some(&windows));
}

#[cfg(test)]
#[path = "window_tests.rs"]
mod tests;
