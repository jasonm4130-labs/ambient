//! Ask macOS what icon it resolves for the built bundle, and write it out.
//! The plist key and the file being present prove neither that the .icns parses
//! nor that LaunchServices picks it up.
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSWorkspace};
use objc2_foundation::{NSDictionary, NSString};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (app, out) = (args[0].clone(), args[1].clone());
    unsafe {
        let icon = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(&app));
        let size = icon.size();
        println!("resolved icon: {}x{} pt", size.width, size.height);
        let reps = icon.representations();
        println!("{} representation(s):", reps.len());
        for r in reps.iter() {
            println!("  {}x{}", r.pixelsWide(), r.pixelsHigh());
        }
        let tiff = icon.TIFFRepresentation().expect("no TIFF");
        let rep = NSBitmapImageRep::imageRepWithData(&tiff).expect("no rep");
        let png = rep
            .representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
            .expect("no png");
        png.writeToFile_atomically(&NSString::from_str(&out), true);
        println!("wrote {out}");
    }
}
