//! Ask macOS whether each menu bar symbol resolves. `set_state` keeps the
//! previous icon when a symbol is missing, so the menu bar would silently show
//! the wrong state.
use ambient::state::PhaseKind;
use objc2_app_kit::NSImage;
use objc2_foundation::NSString;

fn main() {
    let mut missing = 0;
    for name in PhaseKind::ALL.map(PhaseKind::symbol) {
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
