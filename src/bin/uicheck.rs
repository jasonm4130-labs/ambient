//! Drives the built page in a real WKWebView and reports what the bridge
//! receives. Compiling and type-checking prove the page *says* the right
//! thing; only this proves it does anything.
//!
//! A canned responder answers the page's requests the way Rust would —
//! `init`, `sessions`, `transcript`, `export`, `config.get`, `devices`,
//! `speakers.unnamed`, and `{}` for anything else — then drives the page
//! through: selecting a session, clicking Copy Markdown (exercising `export`
//! then `clipboard.write`), opening the export menu and clicking Copy for an
//! assistant (the same two calls, with `format: "assistant"`), navigating to
//! Settings, reading back what Settings rendered, and clicking the diarize
//! switch. Prints every message the bridge receives, and snapshots a PNG so
//! the rendering can be looked at too.
//!
//! Does not click "+ Add", "Change…" or "Save as…": all three open a modal
//! `NSOpenPanel` or `NSSavePanel` that nothing here would dismiss, and the
//! run would hang.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBitmapImageFileType,
    NSBitmapImageRep, NSImage, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSDictionary, NSError, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSString, NSTimer,
};
use objc2_web_kit::{
    WKScriptMessage, WKScriptMessageHandler, WKUserContentController, WKWebView,
    WKWebViewConfiguration,
};
use serde::Deserialize;
use serde_json::{json, Value};

const PAGE: &str = include_str!("../../assets/settings.html");

/// A routed request from the page, the same shape `src/settings.rs` reads:
/// `{id, method, params}`. `params` is not read here — every canned reply is
/// keyed on `method` alone.
#[derive(Debug, Deserialize)]
struct Request {
    id: u64,
    method: String,
}

/// The `config.get` payload this probe hands back: apps non-empty so the
/// scope select renders as "Selected apps" with a chip, and `latest_session`
/// non-null so the page asks `speakers.unnamed` at all.
fn config_payload() -> Value {
    json!({
        "apps": ["us.zoom.xos"],
        "input_device": "MacBook Pro Microphone",
        "diarize": true,
        "threshold": 0.5,
        "sessions_dir": null,
        "devices": ["MacBook Pro Microphone", "Iriun Webcam Audio"],
        "default_dir": "/Users/x/Documents/Ambient",
        "ask_before_recording": true,
        "audio_retention": "7",
        "roster": ["Marcus", "Priya"],
        "latest_session": "2026-08-29T1517",
    })
}

/// The `sessions` payload, in the real wire shape `session::SessionSummary`
/// derives — `live`/`transcribing`/`error`/`transcribed` booleans and **no**
/// `state` field, so this probe would catch a page that assumed one.
fn sessions_payload() -> Value {
    let mut sessions = json!([
        {
            "id": "2026-08-29T1517",
            "dir": "/Users/x/Documents/Ambient/2026-08-29T1517",
            "name": "Design review",
            "started_at": "2026-08-29 15:17",
            "duration_s": 1847.0,
            "transcribed": true,
            "live": false,
            "transcribing": false,
            "error": null,
            "tags": [],
            "pinned": false,
        },
        {
            "id": "2026-08-28T0930",
            "dir": "/Users/x/Documents/Ambient/2026-08-28T0930",
            "name": null,
            "started_at": "2026-08-28 09:30",
            "duration_s": 612.0,
            "transcribed": false,
            "live": false,
            "transcribing": false,
            "error": "session.json was empty",
            "tags": [],
            "pinned": false,
        },
    ]);
    let template = sessions[0].clone();
    for i in 2..2000 {
        let mut session = template.clone();
        session["id"] = json!(format!("archive-{i:04}"));
        session["name"] = json!(format!("Review {i:04}"));
        session["tags"] = json!(["review"]);
        sessions.as_array_mut().unwrap().push(session);
    }
    sessions
}

fn transcript_payload() -> Value {
    json!({
        "session": "2026-08-29T1517",
        "state": "done",
        "next": 2,
        "lines": [
            {"track": "call", "start_ms": 0, "end_ms": 4200, "speaker": "Priya", "text": "shall we start with the export spec"},
            {"track": "room", "start_ms": 4200, "end_ms": 6100, "speaker": "Marcus", "text": "yes, go ahead"},
        ],
    })
}

fn export_payload() -> Value {
    json!({
        "session": "2026-08-29T1517",
        "format": "markdown",
        "text": "# Design review\n\n**Priya** [00:00] shall we start with the export spec\n\n**Marcus** [00:04] yes, go ahead\n",
    })
}

/// What the responder answers `method` with, mirroring what Rust would.
fn canned(method: &str) -> Value {
    match method {
        "init" => json!({"route": "sessions"}),
        "sessions" => sessions_payload(),
        "transcript" => transcript_payload(),
        "export" => export_payload(),
        "config.get" | "config.set" => config_payload(),
        "doctor" => json!([{"name": "config", "ok": true, "detail": "defaults"}]),
        "devices" => json!({"devices": ["MacBook Pro Microphone", "Iriun Webcam Audio"]}),
        "speakers.unnamed" => json!([
            {"label": "call-1", "sample": "shall we start with the export spec"},
            {"label": "room-1", "sample": "yes, go ahead"},
        ]),
        _ => json!({}),
    }
}

struct Ivars {
    seen: RefCell<Vec<String>>,
    web: RefCell<Option<Retained<WKWebView>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AmbientUiCheck"]
    #[ivars = Ivars]
    struct Probe;

    unsafe impl NSObjectProtocol for Probe {}

    unsafe impl WKScriptMessageHandler for Probe {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn did_receive(&self, _c: &WKUserContentController, msg: &WKScriptMessage) {
            let body = unsafe { msg.body() };
            let Ok(s) = body.downcast::<NSString>() else {
                println!("  bridge received a NON-STRING body");
                return;
            };
            let text = s.to_string();
            println!("  bridge received: {text}");
            self.ivars().seen.borrow_mut().push(text.clone());

            if let Ok(req) = serde_json::from_str::<Request>(&text) {
                let reply = json!({"result": canned(&req.method)});
                let js = format!("window.ambient.reply({}, {reply});", req.id);
                if let Some(web) = self.ivars().web.borrow().as_ref() {
                    unsafe {
                        web.evaluateJavaScript_completionHandler(&NSString::from_str(&js), None)
                    };
                }
            }
        }
    }
);

fn main() {
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(960.0, 620.0));
    let window: Retained<NSWindow> = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        )
    };

    let probe = Probe::alloc(mtm).set_ivars(Ivars {
        seen: RefCell::new(Vec::new()),
        web: RefCell::new(None),
    });
    let probe: Retained<Probe> = unsafe { msg_send![super(probe), init] };

    let cfg = unsafe { WKWebViewConfiguration::new(mtm) };
    unsafe {
        cfg.userContentController().addScriptMessageHandler_name(
            ProtocolObject::from_ref(&*probe),
            &NSString::from_str("ambient"),
        );
    }
    let web: Retained<WKWebView> =
        unsafe { WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &cfg) };
    unsafe { web.loadHTMLString_baseURL(&NSString::from_str(PAGE), None) };
    window.setContentView(Some(&web));
    window.center();
    window.makeKeyAndOrderFront(None);
    *probe.ivars().web.borrow_mut() = Some(web.clone());

    let js = |web: &WKWebView, src: &str| unsafe {
        web.evaluateJavaScript_completionHandler(&NSString::from_str(src), None);
    };

    let w = web.clone();
    let p = probe.clone();
    let step = RefCell::new(0u32);
    let snapshot = Rc::new(Cell::new(false));
    let snapshot_done = snapshot.clone();
    let block = RcBlock::new(move |_t: core::ptr::NonNull<NSTimer>| {
        let mut n = step.borrow_mut();
        *n += 1;
        match *n {
            3 => {
                println!("\n[1] selecting a session");
                js(
                    &w,
                    r#"document.querySelector('[data-testid="session-row"]').click();"#,
                );
            }
            4 => {
                println!("\n[2] clicking Copy Markdown");
                js(
                    &w,
                    r#"document.querySelector('[data-testid="copy-markdown"]').click();"#,
                );
            }
            5 => {
                println!("\n[3] opening the export menu");
                js(
                    &w,
                    r#"document.querySelector('[data-testid="export-menu-trigger"]').click();"#,
                );
            }
            6 => {
                println!("\n[4] clicking Copy for an assistant");
                js(
                    &w,
                    r#"document.querySelector('[data-testid="export-copy-assistant"]').click();"#,
                );
            }
            7 => {
                println!("\n[5] injecting a `phase` event (recording)");
                js(
                    &w,
                    r#"window.ambient.event("phase", {
                        kind: "recording",
                        app: "us.zoom.xos",
                        live: {
                            id: "2026-08-29T1517",
                            elapsed_s: 42,
                            room_level: 0.3,
                            call_level: 0.6,
                            audio_arriving: true,
                            status_line: "Recording"
                        },
                        queue: null,
                        failure: null
                    });"#,
                );
            }
            8 => {
                println!("\n[6] reading back the live card the phase event drew");
                js(
                    &w,
                    r#"(() => {
                        const t = (id) => document.querySelector(`[data-testid="${id}"]`);
                        const probe = {
                          card: t("live-card") !== null,
                          clock: t("live-clock")?.textContent ?? null,
                          room: t("live-meter-room")?.getAttribute("data-level") ?? null,
                          call: t("live-meter-call")?.getAttribute("data-level") ?? null,
                        };
                        window.webkit.messageHandlers.ambient.postMessage(JSON.stringify({
                          live_probe: probe,
                        }));
                     })();"#,
                );
            }
            9 => {
                println!("\n[7] navigating to settings");
                js(
                    &w,
                    r#"window.ambient.event("navigate", {page: "settings"});"#,
                );
            }
            10 => {
                println!("\n[8] reading back what the page rendered");
                js(
                    &w,
                    r#"(() => {
                        const t = (id) => document.querySelector(`[data-testid="${id}"]`);
                        const chip = (root) => [...root.querySelectorAll("span")]
                          .map((e) => e.textContent.replace(/×$/, ""));
                        const render = {
                          scope: t("scope-select").value,
                          mic: t("mic-select").value,
                          chips: chip(t("app-chips") ?? document.createElement("div")),
                          diarize: t("diarize-switch").getAttribute("aria-checked"),
                          sensitivity: t("sensitivity-slider").value,
                          folder: t("sessions-dir").textContent,
                          retention: t("retention-select").value,
                          people: chip(t("roster-chips")),
                          naming_rows: document.querySelectorAll('[data-testid="naming-row"]').length,
                        };
                        window.webkit.messageHandlers.ambient.postMessage(JSON.stringify({
                          probe_render: render,
                        }));
                     })();"#,
                );
            }
            11 => {
                println!("\n[9] clicking the diarize switch");
                js(
                    &w,
                    r#"document.querySelector('[data-testid="diarize-switch"]').click();"#,
                );
            }
            12 => {
                let dark = std::env::args().nth(2).as_deref() == Some("dark");
                js(&w, &format!("document.documentElement.classList.toggle('dark', {dark}); window.ambient.event('navigate', {{page: 'sessions'}});"));
            }
            13 => {
                js(
                    &w,
                    r#"(async () => {
                    const viewport = document.querySelector('[data-testid="session-list-viewport"]');
                    let maxMs = 0, maxRows = 0;
                    for (let i = 0; i < 30; i++) {
                        const start = performance.now();
                        viewport.scrollTop = (viewport.scrollHeight - viewport.clientHeight) * i / 29;
                        viewport.dispatchEvent(new Event('scroll', {bubbles: true}));
                        await new Promise(resolve => setTimeout(resolve, 0));
                        maxRows = Math.max(maxRows, document.querySelectorAll('[data-testid="session-row"]').length);
                        void viewport.getBoundingClientRect();
                        maxMs = Math.max(maxMs, performance.now() - start);
                    }
                    const last = !!document.querySelector('[data-session="archive-1999"]');
                    window.webkit.messageHandlers.ambient.postMessage(JSON.stringify({scale_probe: {maxMs, maxRows, last}}));
                    viewport.scrollTop = 0;
                    viewport.dispatchEvent(new Event('scroll', {bubbles: true}));
                })();"#,
                );
            }
            15 => js(
                &w,
                r#"document.querySelector('[data-testid="session-row"]').click();"#,
            ),
            17 => {
                let out = std::env::args()
                    .nth(1)
                    .unwrap_or_else(|| "uicheck.png".into());
                let done = snapshot_done.clone();
                let handler = RcBlock::new(move |img: *mut NSImage, _e: *mut NSError| unsafe {
                    if let Some(img) = img.as_ref() {
                        if let Some(tiff) = img.TIFFRepresentation() {
                            if let Some(rep) = NSBitmapImageRep::imageRepWithData(&tiff) {
                                if let Some(png) = rep.representationUsingType_properties(
                                    NSBitmapImageFileType::PNG,
                                    &NSDictionary::new(),
                                ) {
                                    done.set(
                                        png.writeToFile_atomically(&NSString::from_str(&out), true),
                                    );
                                }
                            }
                        }
                    }
                });
                unsafe { w.takeSnapshotWithConfiguration_completionHandler(None, &handler) };
                std::mem::forget(handler);
            }
            20 => {
                let seen: Vec<Value> = p
                    .ivars()
                    .seen
                    .borrow()
                    .iter()
                    .filter_map(|s| serde_json::from_str(s).ok())
                    .collect();
                let count = |method: &str| seen.iter().filter(|v| v["method"] == method).count();
                let live = seen.iter().find_map(|v| v.get("live_probe"));
                let settings = seen.iter().find_map(|v| v.get("probe_render"));
                let scale = seen.iter().find_map(|v| v.get("scale_probe"));
                let ok = count("clipboard.write") == 2
                    && count("export") == 2
                    && count("config.set") == 1
                    && live.is_some_and(|v| {
                        v["card"] == true
                            && v["clock"] == "00:42"
                            && v["room"] == "0.3"
                            && v["call"] == "0.6"
                    })
                    && settings.is_some_and(|v| {
                        v["scope"] == "some" && v["diarize"] == "true" && v["naming_rows"] == 2
                    })
                    && scale.is_some_and(|v| {
                        v["maxRows"].as_u64().is_some_and(|n| n < 300)
                            && v["maxMs"].as_f64().is_some_and(|ms| ms < 16.0)
                            && v["last"] == true
                    })
                    && snapshot.get();
                println!("{} message(s) reached the bridge", seen.len());
                println!(
                    "uicheck: {} — clipboard, live card, settings, 2,000 rows, snapshot",
                    if ok { "ok" } else { "FAILED" }
                );
                std::process::exit(i32::from(!ok));
            }
            _ => {}
        }
    });
    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.6, true, &block) };
    std::mem::forget(block);

    app.run();
}
