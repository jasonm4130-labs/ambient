//! Ask macOS whether each menu bar symbol actually resolves.
//!
//! `set_state` only sets an image when `imageWithSystemSymbolName` returns one,
//! so a symbol this OS does not have leaves the previous icon in place — the
//! menu bar would then say "recording" while armed, silently. That is this
//! project's recurring failure mode, so it gets a check rather than a hope.
use objc2_app_kit::NSImage;
use objc2_foundation::NSString;

fn main() {
    let mut missing = 0;
    for name in [
        "waveform",
        "waveform.badge.exclamationmark",
        "waveform.circle.fill",
        "hourglass",
    ] {
        let n = NSString::from_str(name);
        let desc = NSString::from_str("Ambient");
        let found =
            NSImage::imageWithSystemSymbolName_accessibilityDescription(&n, Some(&desc)).is_some();
        println!("{:<32} {}", name, if found { "ok" } else { "MISSING" });
        if !found {
            missing += 1;
        }
    }
    if missing > 0 {
        eprintln!("\n{missing} symbol(s) will silently leave the wrong icon showing");
        std::process::exit(1);
    }
}
