//! Drives the built page in a real WKWebView and reports what the bridge
//! receives. Compiling and type-checking prove the page *says* the right
//! thing; only this proves it does anything.
//!
//! A canned responder answers the page's requests the way Rust would —
//! `init`, `config.get`, `devices`, `speakers.unnamed`, and `{}` for anything
//! else — then two steps read back what the page rendered and click the
//! diarize switch, printing every message the bridge receives. Snapshots a
//! PNG so the rendering can be looked at too.
//!
//! Trimmed on purpose: acceptance item 7 asks this responder harness for two
//! things, and both belong to later units (`clipboard.write` after Copy
//! Markdown, unit 4; the live card after an injected `phase`, unit 5). It does
//! not click "+ Add" or "Change…": both open a modal `NSOpenPanel` that
//! nothing here would dismiss, and the run would hang.
use std::cell::RefCell;

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

/// What the responder answers `method` with, mirroring what Rust would.
fn canned(method: &str) -> Value {
    match method {
        "init" => json!({"route": "settings"}),
        "config.get" => config_payload(),
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

    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(600.0, 620.0));
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
    let block = RcBlock::new(move |_t: core::ptr::NonNull<NSTimer>| {
        let mut n = step.borrow_mut();
        *n += 1;
        match *n {
            3 => {
                println!("\n[1] reading back what the page rendered");
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
            4 => {
                println!("\n[2] clicking the diarize switch");
                js(
                    &w,
                    r#"document.querySelector('[data-testid="diarize-switch"]').click();"#,
                );
            }
            5 => {
                let out = std::env::args()
                    .nth(1)
                    .unwrap_or_else(|| "uicheck.png".into());
                let handler = RcBlock::new(move |img: *mut NSImage, _e: *mut NSError| unsafe {
                    if let Some(img) = img.as_ref() {
                        if let Some(tiff) = img.TIFFRepresentation() {
                            if let Some(rep) = NSBitmapImageRep::imageRepWithData(&tiff) {
                                if let Some(png) = rep.representationUsingType_properties(
                                    NSBitmapImageFileType::PNG,
                                    &NSDictionary::new(),
                                ) {
                                    png.writeToFile_atomically(&NSString::from_str(&out), true);
                                }
                            }
                        }
                    }
                });
                unsafe { w.takeSnapshotWithConfiguration_completionHandler(None, &handler) };
                std::mem::forget(handler);
            }
            6 => {
                println!(
                    "\n{} message(s) reached the bridge",
                    p.ivars().seen.borrow().len()
                );
                std::process::exit(0);
            }
            _ => {}
        }
    });
    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.6, true, &block) };
    std::mem::forget(block);

    app.run();
}
