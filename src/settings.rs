//! The settings pane.
//!
//! A `WKWebView` rendering the same page the design canvas draws, living as one
//! of the main window's sibling views rather than in a window of its own. The
//! page is embedded with `include_str!` rather than shipped in the bundle's
//! Resources: launched through LaunchServices the working directory is `/`, and
//! that is the only launch that has the audio-capture grant, so a relative path
//! would break the one path that matters.
//!
//! The bridge carries a **JSON string** in each direction rather than a
//! dictionary. `WKScriptMessage::body` hands back an `AnyObject` that would
//! otherwise need unpicking one `NSDictionary` value at a time; a string goes
//! straight to serde.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertStyle, NSAutoresizingMaskOptions, NSModalResponseOK, NSOpenPanel,
};
use objc2_foundation::{
    MainThreadMarker, NSBundle, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use objc2_web_kit::{
    WKScriptMessage, WKScriptMessageHandler, WKUserContentController, WKWebView,
    WKWebViewConfiguration,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::{self, ApiError};
use crate::config::Config;

const PAGE: &str = include_str!("../assets/settings.html");

/// A routed request from the page: `{id, method, params}`. Anything that
/// fails to parse as this (today, only the `Patch` shape) falls back to
/// [`Bridge::handle`].
#[derive(Debug, Deserialize)]
struct Request {
    id: u64,
    method: String,
    params: Option<Value>,
}

/// The page's held events, before it has called `init`: a plain struct with
/// no objc types in it, so its ordering and idempotence are testable off the
/// main thread and without a `WKWebView`.
#[derive(Default)]
struct Events {
    open: bool,
    held: Vec<(String, Value)>,
}

impl Events {
    /// Records one event. Returns the JS to evaluate now if the queue is
    /// open, or `None` if it was held for [`Events::open`] to deliver later.
    fn record(&mut self, name: &str, payload: &Value) -> Option<String> {
        if self.open {
            Some(event_js(name, payload))
        } else {
            self.held.push((name.to_string(), payload.clone()));
            None
        }
    }

    /// Marks the queue open and returns the JS for everything held, in the
    /// order it was recorded. Idempotent: calling this again on an
    /// already-open queue finds nothing left to take and returns an empty
    /// list — `init` may be called more than once (React's `StrictMode`
    /// double-invokes the mount effect) and a second call must not re-deliver
    /// anything.
    fn open(&mut self) -> Vec<String> {
        self.open = true;
        std::mem::take(&mut self.held)
            .into_iter()
            .map(|(name, payload)| event_js(&name, &payload))
            .collect()
    }
}

/// JSON is not quite a JS expression: U+2028 and U+2029 are legal inside a
/// JSON string and terminate a JS line, and `serde_json` does not escape
/// them. A transcript line is user-controlled text and goes through this
/// path, so every piece of JS handed to `evaluateJavaScript` is built through
/// this function.
fn escape_line_terminators(js: String) -> String {
    js.replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// The JS for one reply: `Ok` becomes `{"result": v}`, and the two
/// [`ApiError`] variants become `{"error": {"kind", "message"}}` with the
/// exact `kind` strings units 3–6 read off the wire.
fn reply_js(id: u64, outcome: Result<Value, ApiError>) -> String {
    let body = match outcome {
        Ok(v) => json!({"result": v}),
        Err(ApiError::InvalidParams(m)) => {
            json!({"error": {"kind": "invalid_params", "message": m}})
        }
        Err(ApiError::Failed(m)) => json!({"error": {"kind": "failed", "message": m}}),
    };
    escape_line_terminators(format!("window.ambient.reply({id}, {body});"))
}

/// The JS for one one-way event.
fn event_js(name: &str, payload: &Value) -> String {
    let name_json = json!(name);
    escape_line_terminators(format!("window.ambient.event({name_json}, {payload});"))
}

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
    /// Whether the app is holding a live recording. Written by the window's
    /// one renderer, from the phase, once a tick — this pane has no state of
    /// its own about what the app is doing, and asking for one would be the
    /// second source of truth the design forbids.
    recording: Cell<bool>,
    /// The route the page's `init` should answer with. Set by
    /// [`SettingsPane::select_settings`]; the default is what a freshly
    /// launched window shows.
    route: Cell<&'static str>,
    /// Events recorded before the page's `init`, delivered once it arrives.
    events: RefCell<Events>,
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
            self.route_message(&s.to_string());
        }
    }
);

impl Bridge {
    /// The one entry point for a message off the wire: a request with an
    /// `id` is routed through `init` or [`api::call`]; anything else (today,
    /// only the old `Patch` shape) falls back to [`Bridge::handle`].
    fn route_message(&self, json: &str) {
        match serde_json::from_str::<Request>(json) {
            Ok(req) => self.answer(req),
            Err(_) => self.handle(json),
        }
    }

    /// `init` is intercepted before [`api::call`], which has no `init` arm:
    /// it answers with the pane's current route and opens the event queue,
    /// delivering everything held. Idempotent — a second `init` re-opens an
    /// already-open queue and drains an empty one, which is what
    /// `StrictMode`'s double-invoked mount effect needs.
    fn answer(&self, req: Request) {
        if req.method == "init" {
            let route = self.ivars().route.get();
            self.eval(&reply_js(req.id, Ok(json!({"route": route}))));
            let held = self.ivars().events.borrow_mut().open();
            for js in held {
                self.eval(&js);
            }
            return;
        }
        let params = req.params.unwrap_or_else(|| json!({}));
        let paths = api::Paths {
            config_file: crate::config::path(),
            roster_file: crate::roster::path(),
            sessions_root: std::env::var_os("AMBIENT_HOME").map(PathBuf::from),
        };
        let result = api::call(&req.method, &params, &paths);
        self.eval(&reply_js(req.id, result));
    }

    /// A one-way notification to the page: `config`, `navigate`, `phase` and
    /// `diarize`. Queued until the page's `init`, then sent straight through.
    pub fn event(&self, name: &str, payload: &Value) {
        let js = self.ivars().events.borrow_mut().record(name, payload);
        if let Some(js) = js {
            self.eval(&js);
        }
    }

    fn eval(&self, js: &str) {
        if let Some(web) = self.ivars().web.borrow().as_ref() {
            unsafe { web.evaluateJavaScript_completionHandler(&NSString::from_str(js), None) };
        }
    }

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
                // Bug 1's root cause, blocked from the other side. A recording
                // writes into the directory it claimed, so moving the folder
                // under it no longer makes the capture unreachable — but the
                // sessions the browser lists, the retention sweep and the next
                // claim all follow the setting, and changing it mid-capture
                // splits one conversation across two folders for no gain.
                if self.ivars().recording.get() {
                    self.refuse(
                        "Ambient is recording",
                        "The sessions folder cannot be changed while a recording is \
                         running.\n\nStop the recording first, and this session will \
                         finish where it started.",
                    );
                } else if let Some(p) = self.pick_dir() {
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

    /// Say no, and say why. The page is a view of the file, so `push` at the
    /// end of `handle` already puts the unchanged folder back on screen —
    /// this is what stops that reading as the click having done nothing.
    fn refuse(&self, title: &str, body: &str) {
        eprintln!("settings: refused — {title}: {body}");
        let mtm = MainThreadMarker::from(self);
        let a = NSAlert::new(mtm);
        a.setAlertStyle(NSAlertStyle::Warning);
        a.setMessageText(&NSString::from_str(title));
        a.setInformativeText(&NSString::from_str(body));
        a.runModal();
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
        self.event("config", &payload);
    }
}

/// The settings pane: the `WKWebView` and the bridge that answers it, kept
/// together because the bridge is the web view's script-message handler and
/// nothing else retains it.
///
/// It used to be a window. Two windows meant two menu bars to keep straight
/// under the activation-policy flip — closing the main one would have stripped
/// the settings window's menu bar out from under it — and one of them showed
/// the naming section for `latest()` alone while the other could name any
/// session. It is a sibling view of the main window's pane now, selected by the
/// Settings row, and the page, the bridge and `Bridge::handle` are otherwise
/// exactly what they were.
pub struct SettingsPane {
    web: Retained<WKWebView>,
    bridge: Retained<Bridge>,
}

impl SettingsPane {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let bridge = Bridge::alloc(mtm).set_ivars(Ivars {
            web: RefCell::new(None),
            stashed: RefCell::new(Vec::new()),
            recording: Cell::new(false),
            route: Cell::new("sessions"),
            events: RefCell::new(Events::default()),
        });
        let bridge: Retained<Bridge> = unsafe { msg_send![super(bridge), init] };

        let cfg = unsafe { WKWebViewConfiguration::new(mtm) };
        unsafe {
            let controller = cfg.userContentController();
            let handler = ProtocolObject::from_ref(&*bridge);
            controller.addScriptMessageHandler_name(handler, &NSString::from_str("ambient"));
        }
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(620.0, 720.0));
        let web: Retained<WKWebView> =
            unsafe { WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &cfg) };
        unsafe { web.loadHTMLString_baseURL(&NSString::from_str(PAGE), None) };
        web.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        web.setHidden(true);
        *bridge.ivars().web.borrow_mut() = Some(web.clone());

        Self { web, bridge }
    }

    /// The view to add as a sibling, hide and frame. Everything else about the
    /// pane is the bridge's business.
    pub fn view(&self) -> &WKWebView {
        &self.web
    }

    /// Re-read the file and repaint. The CLI can change settings behind the
    /// pane's back, so showing it re-reads rather than trusting what it last
    /// drew.
    pub fn refresh(&self) {
        self.bridge.push(&Config::load());
    }

    /// Told from the window's one renderer, off the phase. The bridge refuses
    /// to move the sessions folder while this is set.
    pub fn set_recording(&self, recording: bool) {
        self.bridge.ivars().recording.set(recording);
    }

    /// A one-way notification to the page: `config`, `navigate`, `phase` and
    /// `diarize`. Unit 5's `render` calls this with `phase`.
    pub fn event(&self, name: &str, payload: &Value) {
        self.bridge.event(name, payload);
    }

    /// Sets the route `init` answers with to `settings` and tells a page
    /// already mounted to switch there too. Unit 6's host swap wires
    /// `src/window.rs` to this in place of showing the settings view
    /// directly.
    pub fn select_settings(&self) {
        self.bridge.ivars().route.set("settings");
        self.bridge.event("navigate", &json!({"page": "settings"}));
    }

    /// For the launch log. A `WKWebView` that was re-parented into a view
    /// hierarchy it has never lived in before can be present, unhidden and
    /// correctly framed while showing nothing at all, and that failure is
    /// invisible to everything except asking the page itself.
    pub fn describe(&self) -> String {
        let f = self.web.frame();
        // Both are plain property reads on a view this thread owns.
        let (progress, loading) = unsafe { (self.web.estimatedProgress(), self.web.isLoading()) };
        format!(
            "{}x{} loaded {:.0}%{}",
            f.size.width as i64,
            f.size.height as i64,
            progress * 100.0,
            if loading { " (loading)" } else { "" },
        )
    }
}

/// The bridge's half of the contract with `assets/settings.html`: what the page
/// may send and what happens to a message that is not that. The page is the
/// only writer, so a change here is a change to a wire format whose other end
/// ships in the same binary — these pin it so the break is a red test rather
/// than a line on stderr nobody is reading.
#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<Patch, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn an_empty_message_changes_nothing() {
        let p = parse("{}").expect("an empty object is a valid patch");
        assert!(p.scope.is_none());
        assert!(p.input_device.is_none());
        assert!(p.diarize.is_none());
        assert!(p.threshold.is_none());
        assert!(p.remove_app.is_none());
        assert!(p.action.is_none());
        assert!(p.ask_before_recording.is_none());
        assert!(p.audio_retention.is_none());
        assert!(p.add_person.is_none());
        assert!(p.remove_person.is_none());
        assert!(p.assign.is_none());
    }

    #[test]
    fn a_patch_carries_the_fields_it_names_and_no_others() {
        let p = parse(r#"{"threshold":0.6,"diarize":false}"#).expect("two known fields");
        assert_eq!(p.threshold, Some(0.6));
        assert_eq!(p.diarize, Some(false));
        assert!(p.scope.is_none());
        assert!(p.action.is_none());
    }

    #[test]
    fn an_assignment_carries_the_session_it_was_chosen_in() {
        let p =
            parse(r#"{"assign":{"label":"SPEAKER_00","name":"Ana","session":"2026-09-05-1200"}}"#)
                .expect("a complete assignment");
        let a = p.assign.expect("assign is present");
        assert_eq!(a.label, "SPEAKER_00");
        assert_eq!(a.name, "Ana");
        assert_eq!(a.session, "2026-09-05-1200");
    }

    #[test]
    fn a_half_written_assignment_is_refused_rather_than_half_applied() {
        let e = parse(r#"{"assign":{"label":"a"}}"#).expect_err("name and session are required");
        assert!(
            e.to_string().contains("name"),
            "the error should name the missing field, got {e}"
        );
    }

    #[test]
    fn a_threshold_sent_as_a_string_is_refused() {
        // The page's slider sends a number. A string here means the page and
        // this struct have drifted, and reading "0.6" as 0.6 would hide that.
        parse(r#"{"threshold":"0.6"}"#).expect_err("a string is not a number");
    }

    #[test]
    fn forever_stays_a_word_all_the_way_through_the_bridge() {
        let p = parse(r#"{"audio_retention":"forever"}"#).expect("a retention word");
        assert_eq!(p.audio_retention.as_deref(), Some("forever"));
    }

    #[test]
    fn an_unknown_key_is_ignored_rather_than_failing_the_whole_message() {
        // Today's rule, stated so that adding `deny_unknown_fields` is a
        // deliberate change to this test and not a silent one: a page sending
        // a key this build does not know still gets the rest of its edit
        // applied.
        let p = parse(r#"{"unknown":1,"diarize":true}"#).expect("unknown keys are skipped");
        assert_eq!(p.diarize, Some(true));
    }

    #[test]
    fn events_before_init_are_held_and_delivered_in_order() {
        let mut events = Events::default();
        assert!(events.record("a", &json!(1)).is_none());
        assert!(events.record("b", &json!(2)).is_none());

        let delivered = events.open();
        assert_eq!(
            delivered,
            vec![event_js("a", &json!(1)), event_js("b", &json!(2))]
        );

        // An event emitted after `init` is not held: it comes straight back.
        let js = events.record("c", &json!(3));
        assert_eq!(js, Some(event_js("c", &json!(3))));

        // A second `init` finds nothing left to deliver.
        assert!(events.open().is_empty());
    }

    #[test]
    fn reply_js_shapes_results_and_errors_and_escapes_line_terminators() {
        let ok = reply_js(1, Ok(json!({"a": 1})));
        assert!(ok.contains(r#""result":{"a":1}"#), "got {ok:?}");

        let invalid = reply_js(2, Err(ApiError::InvalidParams("bad shape".into())));
        assert!(
            invalid.contains(r#""error":{"kind":"invalid_params","message":"bad shape"}"#),
            "got {invalid:?}"
        );

        let failed = reply_js(3, Err(ApiError::Failed("could not read it".into())));
        assert!(
            failed.contains(r#""error":{"kind":"failed","message":"could not read it""#),
            "got {failed:?}"
        );

        // U+2028 is legal inside a JSON string and terminates a JS line;
        // `serde_json` does not escape it, so the reply helper must.
        let with_separator = reply_js(4, Ok(json!(format!("a{}b", '\u{2028}'))));
        assert!(
            !with_separator.contains('\u{2028}'),
            "got {with_separator:?}"
        );
        assert!(with_separator.contains("\\u2028"), "got {with_separator:?}");
    }
}
