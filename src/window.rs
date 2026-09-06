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

// ---------------------------------------------------------------------------
// Activation policy
// ---------------------------------------------------------------------------

/// A visible window that is not `except` and is a window a person could be
/// looking at.
///
/// The filter is not cosmetic. `NSApp.windows()` carries AppKit's own
/// furniture — the status item's bar window, the carrier window behind an open
/// `NSMenu` — and every one of those is `isVisible`. Counting them would mean
/// the app never demoted at all. `canBecomeKeyWindow` is what separates them:
/// they are borderless and refuse key, while a panel, an alert and the
/// standard About window all accept it.
fn other_visible_window(
    app: &NSApplication,
    except: Option<&NSWindow>,
) -> Option<Retained<NSWindow>> {
    app.windows().iter().find(|w| {
        let same = except.is_some_and(|e| std::ptr::eq(&**w as *const NSWindow, e));
        !same && w.isVisible() && w.canBecomeKeyWindow()
    })
}

/// Give the Dock icon and the menu bar back, if nothing on screen still needs
/// them.
///
/// Two guards, both of which have to hold:
///
/// * **Another window is up.** Demoting takes the main menu away from whatever
///   is still open — the About panel, an alert — which reads as a broken app.
/// * **A modal session is running.** `NSOpenPanel::runModal`, which the
///   settings page's Choose… buttons use, spins its own run loop; changing the
///   activation policy underneath it is the standard way this pattern
///   deadlocks.
///
/// Nothing here is called on a background event. The only caller is
/// `windowWillClose:`, and minimising does not close.
pub fn demote_to_accessory(mtm: MainThreadMarker, closing: Option<&NSWindow>) {
    let app = NSApplication::sharedApplication(mtm);
    if app.activationPolicy() != NSApplicationActivationPolicy::Regular {
        return;
    }
    // A refusal is worth a log line when it explains a close that did not
    // demote, and pure noise on the retry tick, which asks the same question
    // twice a second for the whole time the window is open. `closing` is
    // exactly that distinction: `windowWillClose:` passes the window, the
    // retry passes None.
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

/// What the activation policy is, for a log line a bundle launch can show.
pub fn policy_name(mtm: MainThreadMarker) -> &'static str {
    match NSApplication::sharedApplication(mtm).activationPolicy() {
        NSApplicationActivationPolicy::Regular => "Regular",
        NSApplicationActivationPolicy::Accessory => "Accessory",
        _ => "Prohibited",
    }
}

// ---------------------------------------------------------------------------
// The application main menu
// ---------------------------------------------------------------------------

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

/// Install a real application main menu.
///
/// Every item is **nil-targeted**, so each one is dispatched down the
/// responder chain: `openSettings:` reaches the app delegate, Copy Markdown
/// and Reveal in Finder reach the main window's delegate, and the Edit menu's
/// standard selectors reach whatever text is being edited — the transcript
/// view, the combo boxes step 6 adds, and the settings `WKWebView`.
///
/// Inert under Accessory, mandatory under Regular: promoting the activation
/// policy without this yields a menu bar holding only the Apple menu, which
/// reads as a broken app. That is why it is installed from this step, before
/// step 8 makes the flip.
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
