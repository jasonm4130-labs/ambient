//! The full-window web page and its native bridge.
//!
//! The page is embedded with `include_str!`, not loaded from Resources: under
//! LaunchServices, the only launch with the audio-capture grant, the working
//! directory is `/`. The bridge carries a JSON string each way, because
//! `WKScriptMessage::body` would otherwise need unpicking one `NSDictionary`
//! value at a time.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

use anyhow::anyhow;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertStyle, NSApplication, NSAutoresizingMaskOptions, NSModalResponseOK,
    NSOpenPanel, NSPasteboard, NSPasteboardTypeString, NSSavePanel, NSWorkspace,
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

#[derive(Debug, Deserialize)]
struct Request {
    id: u64,
    method: String,
    params: Option<Value>,
}

#[derive(Default)]
struct Events {
    open: bool,
    held: Vec<(String, Value)>,
}

impl Events {
    /// The JS to evaluate now, or `None` if held for [`Events::open`].
    fn record(&mut self, name: &str, payload: &Value) -> Option<String> {
        if self.open {
            Some(event_js(name, payload))
        } else {
            self.held.push((name.to_string(), payload.clone()));
            None
        }
    }

    /// Idempotent: React's `StrictMode` double-invokes the mount effect, so
    /// `init` can arrive twice and must not re-deliver anything.
    fn open(&mut self) -> Vec<String> {
        self.open = true;
        std::mem::take(&mut self.held)
            .into_iter()
            .map(|(name, payload)| event_js(&name, &payload))
            .collect()
    }
}

/// `serde_json` leaves U+2028 and U+2029 unescaped, and they end a JS line.
/// Transcript text is user-controlled, so all JS for `evaluateJavaScript` goes
/// through this.
fn escape_line_terminators(js: String) -> String {
    js.replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// The `kind` strings are read by the page.
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

fn event_js(name: &str, payload: &Value) -> String {
    let name_json = json!(name);
    escape_line_terminators(format!("window.ambient.event({name_json}, {payload});"))
}

/// Stashes the app list so a round trip through "Everything" keeps it.
fn switch_scope(apps: &mut Vec<String>, stash: &mut Vec<String>, everything: bool) {
    if everything {
        *stash = std::mem::take(apps);
    } else {
        *apps = std::mem::take(stash);
    }
}

struct Ivars {
    web: RefCell<Option<Retained<WKWebView>>>,
    stashed: RefCell<Vec<String>>,
    /// Written by the window's renderer from the phase; the pane keeps no state
    /// of its own about what the app is doing.
    recording: Cell<bool>,
    route: Cell<&'static str>,
    events: RefCell<Events>,
    /// One job at a time: two would double the model helper's memory.
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
    fn route_message(&self, json: &str) {
        match serde_json::from_str::<Request>(json) {
            Ok(req) => self.answer(req),
            Err(e) => eprintln!("settings: could not parse {json:?} as a request ({e})"),
        }
    }

    /// Every path must reply: an unanswered id is a control that hangs for the
    /// rest of the session. `init` and the bridge-only methods are matched
    /// before [`api::call`], which has no arm for them.
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
            "save" => {
                let result = self.save(req.params.as_ref());
                self.eval(&reply_js(req.id, result));
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

    /// Held until the page's `init`.
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

    fn refuse(&self, title: &str, body: &str) {
        eprintln!("settings: refused — {title}: {body}");
        let mtm = MainThreadMarker::from(self);
        let a = NSAlert::new(mtm);
        a.setAlertStyle(NSAlertStyle::Warning);
        a.setMessageText(&NSString::from_str(title));
        a.setInformativeText(&NSString::from_str(body));
        a.runModal();
    }

    /// Resolves the picked app to its bundle ID, so nobody types `us.zoom.xos`.
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

    /// The export runs before the panel opens, so a bad session or a failed
    /// export replies with an `ApiError` and never shows a modal.
    fn save(&self, params: Option<&Value>) -> Result<Value, ApiError> {
        let params = params.cloned().unwrap_or_else(|| json!({}));
        let session = params
            .get("session")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::InvalidParams("`session` must be a string".to_string()))?;
        let format = params
            .get("format")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::InvalidParams("`format` must be a string".to_string()))?;

        let exported = api::call("export", &params, &self.paths())?;
        let text = exported
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::Failed("export did not return text".to_string()))?;

        let extension = match format {
            "markdown" => "md",
            "text" => "txt",
            "json" => "json",
            "srt" => "srt",
            "vtt" => "vtt",
            other => other,
        };
        let Some(path) = self.run_save_panel(session, extension) else {
            return Ok(json!({"path": Value::Null}));
        };
        std::fs::write(&path, text).map_err(|e| ApiError::Failed(format!("{e}")))?;
        Ok(json!({"path": path.to_string_lossy()}))
    }

    fn run_save_panel(&self, session: &str, extension: &str) -> Option<PathBuf> {
        let mtm = MainThreadMarker::from(self);
        let panel = NSSavePanel::savePanel(mtm);
        panel.setMessage(Some(&NSString::from_str("Save the transcript")));
        panel.setNameFieldStringValue(&NSString::from_str(&format!("{session}.{extension}")));
        if panel.runModal() != NSModalResponseOK {
            return None;
        }
        Some(PathBuf::from(panel.URL()?.path()?.to_string()))
    }

    /// `AMBIENT_HOME` overrides only sessions; config and roster keep their
    /// normal paths.
    fn paths(&self) -> api::Paths {
        api::Paths {
            config_file: crate::config::path(),
            roster_file: crate::roster::path(),
            sessions_root: std::env::var_os("AMBIENT_HOME").map(PathBuf::from),
        }
    }

    /// No `session` reveals the sessions root.
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

    /// Replies with whether `sel` reached anybody, so a selector nobody answers
    /// is visible on the wire.
    fn forward_action(&self, id: u64, sel: Sel) {
        let mtm = MainThreadMarker::from(self);
        let app = NSApplication::sharedApplication(mtm);
        let me: &AnyObject = self;
        let sent = unsafe { app.sendAction_to_from(sel, None, Some(me)) };
        self.eval(&reply_js(id, Ok(json!({"sent": sent}))));
    }

    /// One window-run worker at a time; the session lock excludes the queue,
    /// the CLI and naming while it writes.
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
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_dir = dir.clone();
        std::thread::spawn(move || {
            // The failure count stays, so a retry at the cap still falls back
            // to an unlabelled transcript; success clears it.
            let result = if worker_dir.join("transcript.md").is_file() {
                session::rerun_session(&worker_dir, None)
            } else {
                session::transcribe_session(&worker_dir, None)
            };
            tx.send(result.map(|_| 0)).ok();
        });
        *self.ivars().diarizing.borrow_mut() = Some((session.to_string(), rx));
        Ok(json!({"started": true}))
    }

    /// Emits no `running` event: the page knows it started because its own
    /// `diarize.start` resolved.
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

/// Holds the bridge, the web view's script-message handler, which nothing else
/// retains.
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

    pub fn view(&self) -> &WKWebView {
        &self.web
    }

    /// A nudge with no payload: the CLI can change settings behind the pane, so
    /// the page re-requests `config.get`.
    pub fn refresh(&self) {
        self.bridge.event("config", &json!({}));
    }

    /// The bridge refuses to move the sessions folder while this is set.
    pub fn set_recording(&self, recording: bool) {
        self.bridge.ivars().recording.set(recording);
    }

    pub fn event(&self, name: &str, payload: &Value) {
        self.bridge.event(name, payload);
    }

    pub fn poll_diarize(&self) {
        self.bridge.poll_diarize();
    }

    pub fn select_settings(&self) {
        self.bridge.ivars().route.set("settings");
        self.bridge.event("navigate", &json!({"page": "settings"}));
    }

    pub fn select_sessions(&self) {
        self.bridge.ivars().route.set("sessions");
        self.bridge.event("navigate", &json!({"page": "sessions"}));
    }

    /// For the launch log: a re-parented `WKWebView` can be present, unhidden
    /// and framed while drawing nothing, and only the page itself can tell.
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

/// Pins the wire contract with `assets/settings.html`, so a break is a red
/// test rather than a line on stderr.
#[cfg(test)]
mod tests {
    use super::*;

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

        let js = events.record("c", &json!(3));
        assert_eq!(js, Some(event_js("c", &json!(3))));

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

        let with_separator = reply_js(4, Ok(json!(format!("a{}b", '\u{2028}'))));
        assert!(
            !with_separator.contains('\u{2028}'),
            "got {with_separator:?}"
        );
        assert!(with_separator.contains("\\u2028"), "got {with_separator:?}");
    }
}
