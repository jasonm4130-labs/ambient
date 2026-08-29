//! Drives the built settings page in a real WKWebView and reports what the
//! bridge receives. Compiling and type-checking prove the page *says* the right
//! thing; only this proves it does anything.
//!
//! Loads `assets/settings.html`, pushes a config in through `applyConfig`, then
//! synthesises the clicks a user would make and prints every message the page
//! posts back. Snapshots a PNG so the rendering can be looked at too.
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

const PAGE: &str = include_str!("../../assets/settings.html");

struct Ivars {
    seen: RefCell<Vec<String>>,
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
            match body.downcast::<NSString>() {
                Ok(s) => {
                    let text = s.to_string();
                    println!("  bridge received: {text}");
                    self.ivars().seen.borrow_mut().push(text);
                }
                Err(_) => println!("  bridge received a NON-STRING body"),
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
            2 => {
                println!("\n[1] pushing a config in");
                js(
                    &w,
                    r#"applyConfig({"apps":["us.zoom.xos"],"input_device":"MacBook Pro Microphone",
                       "diarize":true,"threshold":0.5,"sessions_dir":null,
                       "devices":["MacBook Pro Microphone","Iriun Webcam Audio"],
                       "default_dir":"/Users/x/Documents/Ambient"});"#,
                );
            }
            3 => {
                println!("\n[2] reading back what the page rendered");
                js(
                    &w,
                    r#"(() => {
                        const t = (id) => document.getElementById(id);
                        console.log("RENDERED scope=" + t("scope").value
                          + " mic=" + t("mic").value
                          + " chips=" + [...document.querySelectorAll(".chip b")].map(e=>e.textContent).join("/")
                          + " micoptions=" + t("mic").options.length
                          + " diarize=" + t("diarize").classList.contains("on")
                          + " sens=" + t("sens").value
                          + " dir=" + t("dir").textContent);
                        window.webkit.messageHandlers.ambient.postMessage(JSON.stringify({
                          probe_render: t("scope").value + "|" + t("mic").value + "|"
                            + t("sens").value + "|" + t("dir").textContent + "|"
                            + t("diarize").classList.contains("on")
                        }));
                    })();"#,
                );
            }
            4 => {
                println!("\n[3] clicking the diarize switch");
                js(&w, r#"document.getElementById("diarize").click();"#);
            }
            5 => {
                println!("\n[4] dragging the sensitivity slider to the 'More' end");
                js(
                    &w,
                    r#"(() => { const s = document.getElementById("sens");
                       s.value = "80"; s.dispatchEvent(new Event("change")); })();"#,
                );
            }
            6 => {
                println!("\n[5] removing the app chip");
                js(&w, r#"document.querySelector(".chip .x").click();"#);
            }
            7 => {
                println!("\n[6] clicking + Add");
                js(&w, r#"document.querySelector(".add").click();"#);
            }
            8 => {
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
            9 => {
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
