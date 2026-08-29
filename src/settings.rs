//! The settings window.
//!
//! An `NSWindow` holding a `WKWebView`, which renders the same page the design
//! canvas draws. The page is embedded with `include_str!` rather than shipped
//! in the bundle's Resources: launched through LaunchServices the working
//! directory is `/`, and that is the only launch that has the audio-capture
//! grant, so a relative path would break the one path that matters.
//!
//! The bridge carries a **JSON string** in each direction rather than a
//! dictionary. `WKScriptMessage::body` hands back an `AnyObject` that would
//! otherwise need unpicking one `NSDictionary` value at a time; a string goes
//! straight to serde.

use std::cell::RefCell;
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSModalResponseOK, NSOpenPanel, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSBundle, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use objc2_web_kit::{
    WKScriptMessage, WKScriptMessageHandler, WKUserContentController, WKWebView,
    WKWebViewConfiguration,
};
use serde::Deserialize;

use crate::config::Config;

const PAGE: &str = include_str!("../assets/settings.html");

/// One edit from the page. Every field is optional because the page sends only
/// what changed.
#[derive(Debug, Default, Deserialize)]
struct Patch {
    /// "all" or "some" — whether to filter by app at all.
    scope: Option<String>,
    input_device: Option<String>,
    diarize: Option<bool>,
    threshold: Option<f32>,
    remove_app: Option<String>,
    /// "add_app" or "choose_dir": needs a native panel.
    action: Option<String>,
    ask_before_recording: Option<bool>,
    /// A number of days, or "forever". A string rather than an integer so that
    /// keeping audio indefinitely stays a deliberate word on both sides.
    audio_retention: Option<String>,
    add_person: Option<String>,
    remove_person: Option<String>,
    /// Put a roster name on one of the recording's speaker labels.
    assign: Option<Assign>,
}

#[derive(Debug, Deserialize)]
struct Assign {
    label: String,
    name: String,
    /// The session the page was showing when the name was chosen. A recording
    /// can finish while the window is open, and without this the name would
    /// land on whichever session is newest by then — a different conversation.
    session: String,
}

struct Ivars {
    web: RefCell<Option<Retained<WKWebView>>>,
    /// Remembers the app list while "Everything" is selected, so switching to
    /// "Selected apps" and back does not silently discard it.
    stashed: RefCell<Vec<String>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AmbientSettingsBridge"]
    #[ivars = Ivars]
    struct Bridge;

    unsafe impl NSObjectProtocol for Bridge {}

    unsafe impl WKScriptMessageHandler for Bridge {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn did_receive(&self, _c: &WKUserContentController, msg: &WKScriptMessage) {
            let body = unsafe { msg.body() };
            let Ok(s) = body.downcast::<NSString>() else {
                eprintln!("settings: ignoring a message that was not a string");
                return;
            };
            self.handle(&s.to_string());
        }
    }
);

impl Bridge {
    fn handle(&self, json: &str) {
        let patch: Patch = match serde_json::from_str(json) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("settings: could not read {json:?} ({e})");
                return;
            }
        };
        let mut cfg = Config::load();

        if let Some(scope) = &patch.scope {
            if scope == "all" {
                // Keep the list rather than destroying it — the user is
                // switching a mode, not clearing their choices.
                *self.ivars().stashed.borrow_mut() = std::mem::take(&mut cfg.apps);
            } else {
                cfg.apps = std::mem::take(&mut self.ivars().stashed.borrow_mut());
            }
        }
        if let Some(d) = &patch.input_device {
            cfg.input_device = (d != "default").then(|| d.clone());
        }
        if let Some(d) = patch.diarize {
            cfg.diarize = d;
        }
        if let Some(t) = patch.threshold {
            cfg.threshold = t;
        }
        if let Some(id) = &patch.remove_app {
            cfg.apps.retain(|a| a != id);
        }
        if let Some(a) = patch.ask_before_recording {
            cfg.ask_before_recording = a;
        }
        if let Some(d) = &patch.audio_retention {
            if let Err(e) = cfg.set("audio_retention_days", d) {
                eprintln!("settings: {e}");
            }
        }
        if let Some(who) = &patch.add_person {
            let mut names = crate::roster::load();
            if crate::roster::add(&mut names, who) {
                if let Err(e) = crate::roster::save(&names) {
                    eprintln!("settings: could not save the roster ({e})");
                }
            }
        }
        if let Some(who) = &patch.remove_person {
            let mut names = crate::roster::load();
            if crate::roster::remove(&mut names, who) {
                if let Err(e) = crate::roster::save(&names) {
                    eprintln!("settings: could not save the roster ({e})");
                }
            }
        }
        if let Some(a) = &patch.assign {
            // Naming goes through the same path the CLI uses, so the edit is
            // recorded as the user's and survives a re-diarize.
            let dir = crate::session::home().join(&a.session);
            let still_there =
                dir.file_name().is_some_and(|n| n == a.session.as_str()) && dir.is_dir();
            if !still_there {
                // Refuses rather than guessing: putting a name on the wrong
                // recording is worse than putting none on this one.
                eprintln!("settings: {:?} is not a session here", a.session);
            } else {
                match crate::session::name_speaker(&dir, &a.label, &a.name) {
                    Ok(n) => eprintln!("settings: {n} line(s) now attributed to {}", a.name),
                    Err(e) => eprintln!("settings: could not name {}: {e}", a.label),
                }
            }
        }
        match patch.action.as_deref() {
            Some("add_app") => {
                if let Some(id) = self.pick_app() {
                    if !cfg.apps.contains(&id) {
                        cfg.apps.push(id);
                    }
                }
            }
            Some("choose_dir") => {
                if let Some(p) = self.pick_dir() {
                    cfg.sessions_dir = Some(p);
                }
            }
            Some(other) => eprintln!("settings: unknown action {other:?}"),
            None => {}
        }

        if let Err(e) = cfg.save() {
            eprintln!("settings: could not save ({e})");
        }
        self.push(&cfg);
    }

    /// An app bundle, resolved to the bundle ID the tap actually needs. The
    /// user picks *Zoom*; nobody should have to type `us.zoom.xos`.
    fn pick_app(&self) -> Option<String> {
        let mtm = MainThreadMarker::from(self);
        let panel = NSOpenPanel::openPanel(mtm);
        {
            panel.setCanChooseFiles(true);
            panel.setCanChooseDirectories(false);
            panel.setAllowsMultipleSelection(false);
            panel.setMessage(Some(&NSString::from_str(
                "Choose an app to capture audio from",
            )));
            panel.setDirectoryURL(Some(&objc2_foundation::NSURL::fileURLWithPath(
                &NSString::from_str("/Applications"),
            )));
            if panel.runModal() != NSModalResponseOK {
                return None;
            }
            let url = panel.URL()?;
            let bundle = NSBundle::bundleWithURL(&url)?;
            let id = bundle.bundleIdentifier()?.to_string();
            Some(id)
        }
    }

    fn pick_dir(&self) -> Option<PathBuf> {
        let mtm = MainThreadMarker::from(self);
        let panel = NSOpenPanel::openPanel(mtm);
        {
            panel.setCanChooseFiles(false);
            panel.setCanChooseDirectories(true);
            panel.setCanCreateDirectories(true);
            panel.setMessage(Some(&NSString::from_str(
                "Where should sessions be written?",
            )));
            if panel.runModal() != NSModalResponseOK {
                return None;
            }
            Some(PathBuf::from(panel.URL()?.path()?.to_string()))
        }
    }

    /// Hand the whole config back to the page. Always the whole thing, never a
    /// delta: the page is a view and the file is the truth.
    fn push(&self, cfg: &Config) {
        let devices: Vec<String> = crate::capture::input_devices()
            .into_iter()
            .map(|(_, n)| n)
            .collect();
        let home = std::env::var("HOME").unwrap_or_default();
        // The naming section is about one recording — the most recent — so it
        // is empty until there is a session with speakers nobody has named.
        let latest = crate::session::latest(&crate::session::home());
        // `null` when the session could not be read at all, which the page
        // reports as such rather than claiming everyone already has a name.
        let unnamed: Option<Vec<serde_json::Value>> = latest.as_deref().and_then(|d| {
            match crate::session::unnamed_labels(d) {
                Ok(v) => Some(
                    v.into_iter()
                        .map(|(label, sample)| {
                            serde_json::json!({ "label": label, "sample": sample })
                        })
                        .collect(),
                ),
                Err(e) => {
                    eprintln!("settings: could not read {}: {e}", d.display());
                    None
                }
            }
        });
        let payload = serde_json::json!({
            "apps": cfg.apps,
            "input_device": cfg.input_device,
            "diarize": cfg.diarize,
            "threshold": cfg.threshold,
            "sessions_dir": cfg.sessions_dir,
            "devices": devices,
            "default_dir": format!("{home}/Documents/Ambient"),
            "ask_before_recording": cfg.ask_before_recording,
            "audio_retention": match cfg.audio_retention_days {
                None => "forever".to_string(),
                Some(n) => n.to_string(),
            },
            "roster": crate::roster::load(),
            "unnamed": unnamed,
            "latest_session": latest
                .as_deref()
                .and_then(|d| d.file_name())
                .map(|n| n.to_string_lossy().to_string()),
        });
        let js = format!("applyConfig({payload});");
        if let Some(web) = self.ivars().web.borrow().as_ref() {
            unsafe { web.evaluateJavaScript_completionHandler(&NSString::from_str(&js), None) };
        }
    }
}

/// The window, kept alive by whoever calls this — closing it would otherwise
/// deallocate the web view and the bridge with it.
pub struct SettingsWindow {
    window: Retained<NSWindow>,
    _bridge: Retained<Bridge>,
}

impl SettingsWindow {
    pub fn open(mtm: MainThreadMarker) -> Self {
        // Taller than the four-row original, and resizable: the who's-who
        // section grows with the roster and with however many speakers the last
        // recording found, so no fixed height is right for long.
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(620.0, 720.0));
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        let window: Retained<NSWindow> = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe {
            window.setTitle(&NSString::from_str("Ambient Settings"));
            // Closing must not deallocate the window; the menu reopens this one.
            window.setReleasedWhenClosed(false);
            window.setContentMinSize(NSSize::new(520.0, 360.0));
        }

        let bridge = Bridge::alloc(mtm).set_ivars(Ivars {
            web: RefCell::new(None),
            stashed: RefCell::new(Vec::new()),
        });
        let bridge: Retained<Bridge> = unsafe { msg_send![super(bridge), init] };

        let cfg = unsafe { WKWebViewConfiguration::new(mtm) };
        unsafe {
            let controller = cfg.userContentController();
            let handler = ProtocolObject::from_ref(&*bridge);
            controller.addScriptMessageHandler_name(handler, &NSString::from_str("ambient"));
        }
        let web: Retained<WKWebView> =
            unsafe { WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &cfg) };
        unsafe { web.loadHTMLString_baseURL(&NSString::from_str(PAGE), None) };
        *bridge.ivars().web.borrow_mut() = Some(web.clone());

        window.setContentView(Some(&web));
        window.center();

        Self {
            window,
            _bridge: bridge,
        }
    }

    /// Bring it forward. An Accessory-policy app has no menu bar of its own, so
    /// it has to activate explicitly or the window opens behind everything.
    pub fn show(&self, mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        self.window.makeKeyAndOrderFront(None);
    }

    /// Re-read the file and repaint. The CLI can change settings behind the
    /// window's back, so reopening it re-reads rather than trusting what it
    /// last drew.
    pub fn refresh(&self) {
        self._bridge.push(&Config::load());
    }
}
