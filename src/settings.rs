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
use std::sync::mpsc::{Receiver, TryRecvError};

use anyhow::anyhow;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertStyle, NSApplication, NSAutoresizingMaskOptions, NSModalResponseOK,
    NSOpenPanel, NSPasteboard, NSPasteboardTypeString, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSBundle, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSString, NSURL,
};
use objc2_web_kit::{
    WKScriptMessage, WKScriptMessageHandler, WKUserContentController, WKWebView,
    WKWebViewConfiguration,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::{self, ApiError};
use crate::config::Config;
use crate::session;

const PAGE: &str = include_str!("../assets/settings.html");

/// A routed request from the page: `{id, method, params}`. Anything that
/// fails to parse as this shape is logged to stderr and ignored.
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

/// Switches the capture scope between "everything" and "selected apps",
/// stashing (or restoring) the app list so a round trip through "Everything"
/// does not silently discard what was chosen. Pure and cheap to pin — the
/// risky half of `capture.scope` is the `Config::load`/`cfg.save` round trip
/// against the user's real config file, which stays untested either way.
fn switch_scope(apps: &mut Vec<String>, stash: &mut Vec<String>, everything: bool) {
    if everything {
        *stash = std::mem::take(apps);
    } else {
        *apps = std::mem::take(stash);
    }
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
    /// The session currently being diarized on a worker thread, and the
    /// channel its result arrives on. One job at a time, exactly like the
    /// native pane's `diarizing` ivar — two diarizers at once would double
    /// the ONNX memory for no reason, and the two are kept safely apart by
    /// `session::claim_transcription`, which the worker below claims first.
    diarizing: RefCell<Option<(String, Receiver<anyhow::Result<usize>>)>>,
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
    /// `id` is routed through [`Bridge::answer`]; anything that fails to
    /// parse as a request is logged to stderr and ignored.
    fn route_message(&self, json: &str) {
        match serde_json::from_str::<Request>(json) {
            Ok(req) => self.answer(req),
            Err(e) => eprintln!("settings: could not parse {json:?} as a request ({e})"),
        }
    }

    /// Every path out of here must reply: `pick_app`/`pick_dir` run a modal
    /// `NSOpenPanel` synchronously, and under request/response an id that is
    /// never answered is a control that hangs for the rest of the session.
    ///
    /// `init` is intercepted first, since [`api::call`] has no arm for it: it
    /// answers with the pane's current route and opens the event queue,
    /// delivering everything held. Idempotent — a second `init` re-opens an
    /// already-open queue and drains an empty one, which is what
    /// `StrictMode`'s double-invoked mount effect needs. `capture.scope`,
    /// `pick_app` and `pick_dir` are bridge-only methods matched next,
    /// because [`api::call`] has no arm for any of them and would answer
    /// `invalid_params`; everything else goes through [`api::call`].
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
        match req.method.as_str() {
            "capture.scope" => {
                let everything = req
                    .params
                    .as_ref()
                    .and_then(|p| p.get("everything"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let mut cfg = Config::load();
                switch_scope(
                    &mut cfg.apps,
                    &mut self.ivars().stashed.borrow_mut(),
                    everything,
                );
                let result = cfg
                    .save()
                    .map(|()| json!({}))
                    .map_err(|e| ApiError::Failed(format!("{e:#}")));
                self.eval(&reply_js(req.id, result));
            }
            "pick_app" => {
                let chosen = self.pick_app();
                self.eval(&reply_js(req.id, Ok(json!({"chosen": chosen}))));
            }
            "pick_dir" => {
                let chosen = if self.ivars().recording.get() {
                    self.refuse(
                        "Ambient is recording",
                        "The sessions folder cannot be changed while a recording is \
                         running.\n\nStop the recording first, and this session will \
                         finish where it started.",
                    );
                    None
                } else {
                    self.pick_dir()
                };
                self.eval(&reply_js(req.id, Ok(json!({"chosen": chosen}))));
            }
            "clipboard.write" => {
                let text = req
                    .params
                    .as_ref()
                    .and_then(|p| p.get("text"))
                    .and_then(Value::as_str);
                let result = match text {
                    Some(text) => {
                        let pasteboard = NSPasteboard::generalPasteboard();
                        pasteboard.clearContents();
                        unsafe {
                            pasteboard.setString_forType(
                                &NSString::from_str(text),
                                NSPasteboardTypeString,
                            );
                        }
                        Ok(json!({"done": true}))
                    }
                    None => Err(ApiError::InvalidParams("text must be a string".to_string())),
                };
                self.eval(&reply_js(req.id, result));
            }
            "reveal" => {
                let session = req
                    .params
                    .as_ref()
                    .and_then(|p| p.get("session"))
                    .and_then(Value::as_str);
                let result = self.reveal(session);
                self.eval(&reply_js(req.id, result));
            }
            // Each forwards its selector up the responder chain exactly as
            // the native Stop button does today (`src/window.rs:800`), to
            // the app delegate that owns the phase. The reply carries whether
            // the send reached anybody, so a selector that reached nobody is
            // visible on the wire rather than silently doing nothing.
            "record.start" => self.forward_action(req.id, sel!(startRecording:)),
            "record.this_call" => self.forward_action(req.id, sel!(recordThisCall:)),
            "record.decline" => self.forward_action(req.id, sel!(notThisOne:)),
            "record.stop" => self.forward_action(req.id, sel!(stopRecording:)),
            "dismiss" => self.forward_action(req.id, sel!(dismissFailure:)),
            "diarize.start" => {
                let session = req
                    .params
                    .as_ref()
                    .and_then(|p| p.get("session"))
                    .and_then(Value::as_str);
                let result = self.diarize_start(session);
                self.eval(&reply_js(req.id, result));
            }
            _ => {
                let params = req.params.unwrap_or_else(|| json!({}));
                let result = api::call(&req.method, &params, &self.paths());
                self.eval(&reply_js(req.id, result));
            }
        }
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

    /// Say no, and say why.
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

    /// The [`api::Paths`] every `api::call` fallthrough and [`Bridge::reveal`]
    /// build from `AMBIENT_HOME`, so a test run with it set cannot be talked
    /// into touching the real config, roster or sessions folder.
    fn paths(&self) -> api::Paths {
        api::Paths {
            config_file: crate::config::path(),
            roster_file: crate::roster::path(),
            sessions_root: std::env::var_os("AMBIENT_HOME").map(PathBuf::from),
        }
    }

    /// `reveal {session}`: today's `revealInFinder:` (`src/window.rs:803`),
    /// including its `create_dir_all` and its "no selection means the
    /// sessions folder" behaviour — `session` absent or null reveals the root
    /// itself. `api::session_dir` is private to `api.rs` and stays that way,
    /// so the id is validated inline the same way it does: reject empty,
    /// `.`, `..`, and any id containing `/`. The root this joins onto is
    /// [`Bridge::paths`]'s, not [`crate::session::home`], so `AMBIENT_HOME`
    /// still governs it under test.
    fn reveal(&self, session: Option<&str>) -> Result<Value, ApiError> {
        let root = self.paths().root();
        let target = match session {
            None => root,
            Some(id) => {
                if id.is_empty() || id == "." || id == ".." || id.contains('/') {
                    return Err(ApiError::InvalidParams(format!(
                        "{id:?} is not a session id"
                    )));
                }
                root.join(id)
            }
        };
        std::fs::create_dir_all(&target).ok();
        let s = NSString::from_str(&target.to_string_lossy());
        if let Some(url) = NSURL::fileURLWithPath(&s).into() {
            let url: Retained<NSURL> = url;
            let urls = NSArray::from_slice(&[&*url]);
            NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&urls);
        }
        Ok(json!({}))
    }

    /// Send `sel` up the responder chain to whoever answers for it — the app
    /// delegate, which owns the `PhaseCell` — and reply with whether it
    /// reached anybody. `startRecording:`, `recordThisCall:`, `notThisOne:`,
    /// `stopRecording:` and `dismissFailure:` all take no target-specific
    /// argument, so this one helper covers every button behind `record.*`
    /// and `dismiss`.
    fn forward_action(&self, id: u64, sel: Sel) {
        let mtm = MainThreadMarker::from(self);
        let app = NSApplication::sharedApplication(mtm);
        let me: &AnyObject = self;
        let sent = unsafe { app.sendAction_to_from(sel, None, Some(me)) };
        self.eval(&reply_js(id, Ok(json!({"sent": sent}))));
    }

    /// `diarize.start {session}`: the same `session::diarize_session` worker
    /// `separateVoices:` spawns (`src/window.rs:892-895`), owned by the
    /// bridge instead of the window. One job at a time, like the native
    /// pane's own `diarizing` ivar — a request while any session is
    /// separating is refused rather than queued. The worker claims
    /// `session::claim_transcription` first, which is what keeps this job
    /// and the native pane's from racing on the same directory.
    fn diarize_start(&self, session: Option<&str>) -> Result<Value, ApiError> {
        let session =
            session.ok_or_else(|| ApiError::InvalidParams("`session` must be a string".into()))?;
        if session.is_empty() || session == "." || session == ".." || session.contains('/') {
            return Err(ApiError::InvalidParams(format!(
                "{session:?} is not a session id"
            )));
        }
        if let Some((current, _)) = self.ivars().diarizing.borrow().as_ref() {
            return Err(ApiError::Failed(format!("already separating {current}")));
        }
        let dir = self.paths().root().join(session);
        let threshold = Config::load().threshold;
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_dir = dir.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let _lock = session::claim_transcription(&worker_dir)?;
                session::diarize_session(&worker_dir, threshold)
            })();
            tx.send(result).ok();
        });
        *self.ivars().diarizing.borrow_mut() = Some((session.to_string(), rx));
        Ok(json!({"started": true}))
    }

    /// Drain the diarize worker's channel, the same way the recording
    /// worker's receiver is polled from `menubar.rs`'s timer tick — a
    /// channel, not a flag file. Called from `SessionList::render`, beside
    /// the `phase` event. Emits no `running` event: the page already knows
    /// it started, because its own `diarize.start` resolved.
    pub fn poll_diarize(&self) {
        let done = {
            let d = self.ivars().diarizing.borrow();
            match d.as_ref() {
                Some((_, rx)) => match rx.try_recv() {
                    Ok(r) => Some(r),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => {
                        Some(Err(anyhow!("the diarize worker thread went away")))
                    }
                },
                None => None,
            }
        };
        let Some(result) = done else { return };
        let session = self
            .ivars()
            .diarizing
            .borrow_mut()
            .take()
            .map(|(s, _)| s)
            .unwrap_or_default();
        let (state, error) = match result {
            Ok(_) => ("done", None),
            Err(e) => ("failed", Some(format!("{e:#}"))),
        };
        self.event(
            "diarize",
            &json!({"session": session, "state": state, "error": error}),
        );
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
/// session. It is a sibling view of the main window's pane now, selected by
/// the Settings row.
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
            diarizing: RefCell::new(None),
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

    /// A nudge, not a payload. The CLI can change settings behind the pane's
    /// back, so showing it tells the page to re-request `config.get` rather
    /// than trusting what it last drew — the page's rule is "re-request after
    /// every event", so this event carries nothing.
    pub fn refresh(&self) {
        self.bridge.event("config", &json!({}));
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

    /// Forwards to the bridge's own diarize poll. `window.rs` only holds the
    /// pane, so `SessionList::render` reaches the bridge's job through here,
    /// beside the `event("phase", …)` call.
    pub fn poll_diarize(&self) {
        self.bridge.poll_diarize();
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

    // The seven tests that used to live here — `an_empty_message_changes_nothing`,
    // `a_patch_carries_the_fields_it_names_and_no_others`,
    // `an_assignment_carries_the_session_it_was_chosen_in`,
    // `a_half_written_assignment_is_refused_rather_than_half_applied`,
    // `a_threshold_sent_as_a_string_is_refused`,
    // `forever_stays_a_word_all_the_way_through_the_bridge`,
    // `an_unknown_key_is_ignored_rather_than_failing_the_whole_message` — pinned
    // the `Patch`/`Assign` wire shape. They are deleted alongside `Patch` and
    // `Assign` in this unit: the outcome's one sanctioned exception to "no
    // editing existing tests" (spec acceptance item 6).

    #[test]
    fn switching_to_everything_stashes_the_apps_in_order_and_back_restores_them() {
        let mut apps = vec!["us.zoom.xos".to_string(), "com.apple.FaceTime".to_string()];
        let mut stash = Vec::new();

        switch_scope(&mut apps, &mut stash, true);
        assert!(apps.is_empty(), "everything empties the app list");
        assert_eq!(
            stash,
            vec!["us.zoom.xos".to_string(), "com.apple.FaceTime".to_string()]
        );

        switch_scope(&mut apps, &mut stash, false);
        assert_eq!(
            apps,
            vec!["us.zoom.xos".to_string(), "com.apple.FaceTime".to_string()]
        );
        assert!(stash.is_empty());
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
