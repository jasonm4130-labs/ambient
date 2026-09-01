//! The session browser: one window, a sidebar of sessions, a transcript, and
//! — pinned above every past session — the recording happening right now.
//!
//! What ships here is the thing `ambient show` and `ambient export` were
//! standing in for, plus the one piece of the design that is
//! about safety rather than convenience — the **warnings banner**. A denied
//! system-audio tap does not fail, it returns correctly shaped zeros, and the
//! advice about that reaches only `eprintln!`, which a bundle launch discards.
//! `SessionMeta.warnings` carries it into `session.json`; this window is where
//! a person finally sees it.
//!
//! Three rules the table plumbing forced, learned from the spike this replaces:
//!
//! 1. `tableView:viewForTableColumn:row:` must be declared with
//!    `#[unsafe(method_id(...))]`. `Retained<NSView>` is not `Encode`, so
//!    `method` does not compile.
//! 2. Inside a `method_id` body neither `?` nor `return` works — the macro
//!    rewrites it into a function returning an opaque value — so the body here
//!    is one call to an ordinary method that may use both.
//! 3. `unsafe impl NSControlTextEditingDelegate for SessionList {}` is
//!    required. It is a supertrait of `NSTableViewDelegate`, and nothing
//!    compiles without it.
//!
//! And one the spike found that the design did not have: `reloadData` clears
//! the selection silently — `selectedRow` goes to `-1` and
//! `tableViewSelectionDidChange:` does *not* fire — so every reload is
//! followed by an explicit re-selection, **by session id and never by row
//! index**. Step 7 pins a live row at index 0, which shifts every other row.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::SystemTime;

use anyhow::anyhow;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSBezelStyle, NSButton, NSColor, NSComboBox, NSControlTextEditingDelegate,
    NSEventModifierFlags, NSFont, NSFontAttributeName, NSFontWeightMedium,
    NSForegroundColorAttributeName, NSLevelIndicator, NSLevelIndicatorStyle, NSLineBreakMode,
    NSMenu, NSMenuItem, NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSPasteboard,
    NSPasteboardTypeString, NSScrollView, NSSegmentSwitchTracking, NSSegmentedControl, NSSplitView,
    NSSplitViewDividerStyle, NSTableColumn, NSTableView, NSTableViewDataSource,
    NSTableViewDelegate, NSTableViewStyle, NSTextField, NSTextView,
    NSUserInterfaceItemIdentification, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
    NSWorkspace,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSArray, NSAttributedString, NSAttributedStringKey, NSDictionary,
    NSIndexSet, NSInteger, NSMutableAttributedString, NSNotification, NSObject, NSObjectProtocol,
    NSPoint, NSRect, NSSize, NSString, NSUInteger, NSURL,
};

use crate::session::{self, MeterPhase, SessionMeta};
use crate::settings::SettingsPane;
use crate::state::{Live, Phase, PhaseKind};

/// Reuse identifier for the sidebar's cell views. Recycling is the part of a
/// view-based table the spike never reached — it only ever had five rows — so
/// the reuse path is written explicitly rather than left to chance.
const CELL: &str = "AmbientSessionRow";

const SIDEBAR_WIDTH: f64 = 274.0;
const ROW_HEIGHT: f64 = 46.0;
const BAR_HEIGHT: f64 = 32.0;
const BANNER_HEIGHT: f64 = 56.0;
const INSET: f64 = 12.0;
/// Height of one naming-strip row (label + combo box + Name button).
const NAMING_ROW_HEIGHT: f64 = 28.0;
/// Height of the Separate-voices row at the very bottom of the pane.
const FOOTER_HEIGHT: f64 = 32.0;

// ---------------------------------------------------------------------------
// The model behind the sidebar
// ---------------------------------------------------------------------------

/// Everything a sidebar row and the pane's heading need about one session.
///
/// Built by [`describe`], which parses `raw.jsonl` and `edits.jsonl`, so it is
/// cached against the newest mtime among the files that could change it —
/// `unnamed_labels` alone would otherwise be re-parsed twice a second for
/// every session in the folder.
struct Detail {
    title: String,
    subtitle: String,
    /// Audio was captured and no transcript was ever written, nothing is
    /// writing to the scratch wav now, and the run never reached the end.
    /// Out of scope to *recover* — the design says so explicitly — but not to
    /// say so.
    interrupted: bool,
    /// The audio is written and the transcript is not: the session is on the
    /// menu bar app's transcription queue, or was when the app last ran.
    queued: bool,
    /// The run reached its end: `transcript.md` is the last thing the
    /// transcriber writes, so its presence separates a session still being
    /// worked on from one that finished and simply heard nothing.
    completed: bool,
    warnings: Vec<String>,
    has_transcript: bool,
    /// What the cache is keyed on, and what tells the pane its rendered
    /// transcript has gone stale.
    stamp: SystemTime,
}

enum Row {
    Session {
        dir: PathBuf,
        id: String,
        detail: Rc<Detail>,
    },
    /// The recording happening right now, pinned at index 0. It carries no
    /// text of its own: the clock and the levels come from [`LiveShot`] at
    /// draw time, so a second of elapsed time does not have to look like a
    /// change to the row *set*.
    ///
    /// Its id is the id of the directory the recording claimed, which is what
    /// makes the selection survive the recording ending: the row stops being
    /// live and the same id turns up again as an ordinary session.
    Live { dir: PathBuf, id: String },
    /// The settings pane, pinned last. A selectable row like any other since
    /// step 8: it shows the `WKWebView` sibling instead of a transcript, where
    /// it used to fire `openSettings:` at a window of its own.
    Settings,
}

/// What the sidebar has selected. Not a row index — `reloadData` drops the
/// selection without telling anyone and the live row shifts every index below
/// it — and not a bare id either, because Settings is a row with no session
/// behind it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Selection {
    /// A session, live or finished, by the id of the directory it claimed.
    Session(String),
    Settings,
}

impl Selection {
    fn session(&self) -> Option<&str> {
        match self {
            Selection::Session(id) => Some(id),
            Selection::Settings => None,
        }
    }
}

/// What the window needs to know about a live recording, read off the shared
/// [`session::Meter`] once per `render` and then treated as a value.
///
/// Every number here is an atomic the worker thread writes each second. None
/// of it is a walk of the sessions folder and none of it is a parse of the
/// free-text status line — that derivation is what bug 2 was, and taking the
/// display off the filesystem is what let it be deleted rather than narrowed.
#[derive(Clone, PartialEq)]
struct LiveShot {
    id: String,
    dir: PathBuf,
    /// The call this recording belongs to, when the watcher started it.
    app: Option<String>,
    elapsed_s: u64,
    /// Per-interval peaks, 0–1. Deliberately not the cumulative peaks
    /// `silent_tap_advice` reads: those never fall, so a meter fed from them
    /// would climb once and stick, and read healthy over a dead microphone.
    room: f64,
    call: f64,
    audio_arriving: bool,
    /// What the *worker* is doing. The Rust phase governs which transitions
    /// are legal; this governs what the user is told.
    meter: MeterPhase,
    /// Whether Stop is a legal move — the Rust phase's business, not the
    /// meter's, and exactly the rule the menu's Stop item uses.
    stoppable: bool,
}

impl LiveShot {
    fn of(live: &Live, kind: PhaseKind) -> LiveShot {
        let m = &live.meter;
        let dir = live.dir.path().to_path_buf();
        LiveShot {
            id: dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            dir,
            app: live.app.clone(),
            elapsed_s: m.elapsed_ms.load(Ordering::Relaxed) / 1000,
            room: m.room_peak_milli.load(Ordering::Relaxed) as f64 / 1000.0,
            call: m.call_peak_milli.load(Ordering::Relaxed) as f64 / 1000.0,
            audio_arriving: m.audio_arriving.load(Ordering::Relaxed),
            meter: m.phase(),
            stoppable: kind == PhaseKind::Recording,
        }
    }

    fn clock(&self) -> String {
        format!("{:02}:{:02}", self.elapsed_s / 60, self.elapsed_s % 60)
    }

    /// What the worker is doing, in the words the sidebar row and the Stop
    /// button both use.
    fn verb(&self) -> &'static str {
        match self.meter {
            MeterPhase::Capturing => "Recording",
            MeterPhase::Transcribing => "Transcribing",
            MeterPhase::Diarizing => "Separating voices",
            MeterPhase::Done => "Finishing",
            MeterPhase::Failed => "Failed",
        }
    }

    /// The sidebar row's two lines. Pure, so the wording is testable without
    /// a window.
    fn row_lines(&self) -> (String, String) {
        let title = match self.meter {
            MeterPhase::Capturing => format!("● Recording — {}", self.clock()),
            _ => format!("● {}…", self.verb()),
        };
        let mut parts = vec![self.capturing_line()];
        if self.meter == MeterPhase::Capturing && !self.audio_arriving {
            parts.push("no audio arriving".into());
        }
        (title, parts.join(" · "))
    }

    /// "Capturing Zoom → 2026-08-30T1412": what is being recorded and where
    /// it is going. The app is the bundle id the watcher armed on, which is
    /// the only name the app actually knows.
    fn capturing_line(&self) -> String {
        // Past tense once the tap has stopped: the worker is still working,
        // but nothing is being captured any more, and a line still saying
        // "Capturing" would be the display lying about the microphone.
        let verb = if self.meter == MeterPhase::Capturing {
            "Capturing"
        } else {
            "Captured"
        };
        match &self.app {
            Some(app) => format!("{verb} {app} → {}", self.id),
            None => format!("Started by hand → {}", self.id),
        }
    }
}

/// The newest mtime among the files whose contents `describe` reads. The
/// directory's own mtime is not enough: appending to `edits.jsonl` — which is
/// exactly what naming a speaker does — does not touch it.
fn stamp_of(dir: &Path) -> SystemTime {
    let mut newest = SystemTime::UNIX_EPOCH;
    for p in [
        dir.to_path_buf(),
        dir.join("raw.jsonl"),
        dir.join("edits.jsonl"),
        dir.join("session.json"),
        // Rewritten as a queued session moves from captured to transcribing
        // to done, which is what the row's subtitle follows.
        dir.join(session::STATUS_FILE),
    ] {
        if let Ok(t) = std::fs::metadata(&p).and_then(|m| m.modified()) {
            if t > newest {
                newest = t;
            }
        }
    }
    newest
}

fn meta_of(dir: &Path) -> Option<SessionMeta> {
    std::fs::read_to_string(dir.join("session.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

/// When the session started: the stored timestamp when there is one, else the
/// directory name, which is a `%Y-%m-%dT%H%M` stamp by construction.
fn started_at(id: &str, meta: Option<&SessionMeta>) -> Option<DateTime<Local>> {
    if let Some(t) = meta
        .and_then(|m| DateTime::parse_from_rfc3339(&m.started_at).ok())
        .map(|t| t.with_timezone(&Local))
    {
        return Some(t);
    }
    let head = id.get(..15)?;
    NaiveDateTime::parse_from_str(head, "%Y-%m-%dT%H%M")
        .ok()
        .and_then(|n| Local.from_local_datetime(&n).single())
}

fn relative(t: DateTime<Local>) -> String {
    let days = (Local::now().date_naive() - t.date_naive()).num_days();
    match days {
        0 => format!("Today {}", t.format("%H:%M")),
        1 => format!("Yesterday {}", t.format("%H:%M")),
        2..=6 => t.format("%a %H:%M").to_string(),
        _ => t.format("%-d %b %Y").to_string(),
    }
}

fn mmss(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

fn audio_present(dir: &Path) -> bool {
    std::fs::read_dir(dir.join("audio"))
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
}

/// Read one session well enough to draw it.
fn describe(dir: &Path) -> Detail {
    let stamp = stamp_of(dir);
    let id = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let meta = meta_of(dir);

    let lines = session::transcript(dir, false).unwrap_or_default();
    let has_transcript = !lines.is_empty();
    let mut speakers: Vec<&str> = Vec::new();
    for s in lines.iter().filter_map(|l| l.speaker.as_deref()) {
        if !speakers.contains(&s) {
            speakers.push(s);
        }
    }
    let unnamed = session::unnamed_labels(dir).map(|v| v.len()).unwrap_or(0);

    // Four things have to be true at once, and the first one is subtle.
    // `raw.jsonl` is created the instant transcription begins and filled
    // incrementally, so its *absence* is what marks a process killed
    // mid-recording. Testing for transcript *lines* instead would mark every
    // healthy session Interrupted for the whole of its own ASR pass — minutes,
    // on the happy path. `is_growing` separates a recording still in flight
    // from a killed one that left its scratch wav behind for ever; the missing
    // `session.json` separates *that* from a capture that ran to its end,
    // whose audio is safe and whose transcript is either queued or written.
    let captured = meta.is_some();
    // `transcript.md` is the last thing the transcriber writes, and the same
    // test `sweep_audio` uses for "finished".
    let completed = dir.join("transcript.md").is_file();
    let interrupted = !dir.join("raw.jsonl").exists()
        && !captured
        && audio_present(dir)
        && !session::is_growing(&dir.join("audio").join("room.native.wav"));
    // Captured and not yet claimed by a transcriber: waiting on the queue.
    let queued = captured && !dir.join("raw.jsonl").exists() && !completed;

    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = started_at(&id, meta.as_ref()) {
        parts.push(relative(t));
    }
    if let Some(m) = &meta {
        parts.push(mmss(m.duration_s));
    }
    if interrupted {
        parts.push("Interrupted".into());
    } else if queued {
        parts.push("waiting to transcribe".into());
    } else if !has_transcript && completed {
        parts.push("no speech recognised".into());
    } else if !has_transcript {
        parts.push("no transcript yet".into());
    }
    match speakers.len() {
        0 => {}
        1 => parts.push("1 speaker".into()),
        n => parts.push(format!("{n} speakers")),
    }
    if unnamed > 0 {
        parts.push(format!("{unnamed} to name"));
    }

    Detail {
        title: meta
            .as_ref()
            .and_then(|m| m.name.clone())
            .unwrap_or_else(|| id.clone()),
        subtitle: parts.join(" · "),
        interrupted,
        queued,
        completed,
        warnings: meta.map(|m| m.warnings).unwrap_or_default(),
        has_transcript,
        stamp,
    }
}

// ---------------------------------------------------------------------------
// Attributed text
// ---------------------------------------------------------------------------

fn attrs(
    font: &NSFont,
    color: &NSColor,
    truncating: bool,
) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    let f: &AnyObject = font;
    let c: &AnyObject = color;
    if truncating {
        // `labelWithAttributedString:` takes its line-break mode from the
        // string, so a two-paragraph label truncates each line instead of
        // wrapping the title over the top of the detail line.
        let p = NSMutableParagraphStyle::new();
        p.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        let p: &AnyObject = &p;
        NSDictionary::from_slices(
            &[
                unsafe { NSFontAttributeName },
                unsafe { NSForegroundColorAttributeName },
                unsafe { NSParagraphStyleAttributeName },
            ],
            &[f, c, p],
        )
    } else {
        NSDictionary::from_slices(
            &[unsafe { NSFontAttributeName }, unsafe {
                NSForegroundColorAttributeName
            }],
            &[f, c],
        )
    }
}

fn run(
    text: &str,
    font: &NSFont,
    color: &NSColor,
    truncating: bool,
) -> Retained<NSAttributedString> {
    let d = attrs(font, color, truncating);
    unsafe {
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &NSString::from_str(text),
            Some(&d),
        )
    }
}

/// Title over a grey detail line, as one attributed string in one text field.
/// A hand-assembled `NSTableCellView` would buy an image view and cost a
/// second view hierarchy to keep in step.
fn row_text(detail: &Detail) -> Retained<NSAttributedString> {
    let out = NSMutableAttributedString::new();
    let title_color = if detail.interrupted {
        NSColor::systemOrangeColor()
    } else {
        NSColor::labelColor()
    };
    out.appendAttributedString(&run(
        &format!("{}\n", detail.title),
        &NSFont::systemFontOfSize(13.0),
        &title_color,
        true,
    ));
    out.appendAttributedString(&run(
        &detail.subtitle,
        &NSFont::systemFontOfSize(11.0),
        &NSColor::secondaryLabelColor(),
        true,
    ));
    Retained::into_super(out)
}

/// The live row, in the same two-line shape as every other row — that is the
/// whole trick behind having one selection model instead of two windows. Red
/// while the tap is actually running, and back to ordinary ink once the
/// worker has moved on to transcribing, because at that point nothing is
/// being captured any more.
fn live_row_text(live: &LiveShot) -> Retained<NSAttributedString> {
    let (title, subtitle) = live.row_lines();
    let color = if live.meter == MeterPhase::Capturing {
        NSColor::systemRedColor()
    } else {
        NSColor::labelColor()
    };
    let out = NSMutableAttributedString::new();
    out.appendAttributedString(&run(
        &format!("{title}\n"),
        &NSFont::systemFontOfSize(13.0),
        &color,
        true,
    ));
    out.appendAttributedString(&run(
        &subtitle,
        &NSFont::systemFontOfSize(11.0),
        &NSColor::secondaryLabelColor(),
        true,
    ));
    Retained::into_super(out)
}

/// The transcript itself. One `NSTextView`, not one table row per `Line`:
/// naming targets a *label*, not a line, so per-line identity buys nothing
/// here and costs a second table.
fn transcript_text(dir: &Path, detail: &Detail, verbatim: bool) -> Retained<NSAttributedString> {
    let body = NSFont::systemFontOfSize(13.0);
    let head = NSFont::boldSystemFontOfSize(12.0);
    let grey = NSColor::secondaryLabelColor();
    let ink = NSColor::labelColor();
    let out = NSMutableAttributedString::new();

    // A missing `raw.jsonl` is not an error worth showing: it is precisely
    // what an interrupted session *is*, and the note below explains it in
    // words. Anything else — a corrupt `edits.jsonl`, an unreadable file — is
    // reported, because silently drawing an empty pane over it would be the
    // same class of failure as the stderr nobody sees.
    let (lines, failure) = match session::transcript(dir, verbatim) {
        Ok(l) => (l, None),
        Err(_) if !dir.join("raw.jsonl").is_file() => (Vec::new(), None),
        Err(e) => (Vec::new(), Some(format!("{e:#}"))),
    };
    // Verbatim means the bytes the recogniser produced, so the bleed filter is
    // part of tidying and not applied to it.
    let (lines, suppressed) = if verbatim {
        (lines, 0)
    } else {
        session::dedup_bleed(&lines)
    };

    if lines.is_empty() {
        let note: &str = if let Some(e) = &failure {
            e
        } else if detail.interrupted {
            "This session has audio but no transcript. The recording was \
             interrupted before it finished — the audio is still in the \
             session folder."
        } else if detail.queued {
            "The audio is written and waiting to be transcribed. The \
             transcript will appear here when the queue reaches it."
        } else if detail.completed {
            "This recording finished, and no speech was recognised in it. \
             If that is a surprise, the capture warnings above are the first \
             thing to read."
        } else if detail.has_transcript {
            "Nothing to show."
        } else {
            "No transcript in this session yet."
        };
        out.appendAttributedString(&run(note, &body, &grey, false));
        return Retained::into_super(out);
    }

    for l in &lines {
        let secs = l.start_ms / 1000;
        let who = l
            .speaker
            .clone()
            .unwrap_or_else(|| l.track.as_str().to_string());
        out.appendAttributedString(&run(
            &format!("[{:02}:{:02}]  {who}\n", secs / 60, secs % 60),
            &head,
            &grey,
            false,
        ));
        out.appendAttributedString(&run(&format!("{}\n\n", l.text), &body, &ink, false));
    }
    if suppressed > 0 {
        out.appendAttributedString(&run(
            &format!(
                "\n{suppressed} room line(s) hidden as microphone bleed. Switch to \
                 Verbatim to see them."
            ),
            &NSFont::systemFontOfSize(11.0),
            &grey,
            false,
        ));
    }
    Retained::into_super(out)
}

// ---------------------------------------------------------------------------
// The window's one Objective-C class
// ---------------------------------------------------------------------------

/// Every view the window owns, built before the class instance exists so the
/// ivars can hold them outright rather than as a pile of `Option`s that are
/// only ever `None` for the length of one function.
struct Views {
    table: Retained<NSTableView>,
    pane: Retained<NSView>,
    bar: Retained<NSView>,
    heading: Retained<NSTextField>,
    modes: Retained<NSSegmentedControl>,
    reveal: Retained<NSButton>,
    copy: Retained<NSButton>,
    banner: Retained<NSTextField>,
    scroll: Retained<NSScrollView>,
    text: Retained<NSTextView>,
    /// Always present once a session is selected; explained rather than just
    /// greyed when there is nothing left to diarize.
    separate: Retained<NSButton>,
    separate_note: Retained<NSTextField>,

    /// The live pane: the fourth sibling in the design's list, toggled by
    /// `setHidden` like the rest and never rebuilt. Shown in place of the
    /// transcript when the live row is the selected one.
    live: Retained<NSView>,
    live_elapsed: Retained<NSTextField>,
    live_caption: Retained<NSTextField>,
    /// Captions set once at construction and never again — they are the only
    /// text in the window that does not depend on state, and `render` writing
    /// a constant would be noise rather than a single source of truth.
    live_room_label: Retained<NSTextField>,
    live_call_label: Retained<NSTextField>,
    live_room: Retained<NSLevelIndicator>,
    live_call: Retained<NSLevelIndicator>,
    live_warning: Retained<NSTextField>,
    /// Nil-targeted on purpose: the action travels the responder chain to the
    /// app delegate, which is where the menu's Stop item sends it too. One
    /// code path, two views.
    live_stop: Retained<NSButton>,
}

/// One row of the naming strip: a label naming diarization's guess, an
/// editable combo box seeded from the roster, and the button that commits it.
/// Rebuilt only when the selected session or its `unnamed_labels` change —
/// never on every timer tick, which would wipe out a name mid-type.
struct NamingRow {
    field: Retained<NSTextField>,
    combo: Retained<NSComboBox>,
    button: Retained<NSButton>,
    label: String,
}

struct Ivars {
    views: Views,
    rows: RefCell<Vec<Row>>,
    cache: RefCell<HashMap<PathBuf, Rc<Detail>>>,
    /// The selection. Row indexes are not identity: `reloadData` drops the
    /// selection without telling anyone, and the live row shifts every index
    /// below it.
    selected: RefCell<Option<Selection>>,
    /// The settings page, re-parented here in step 8 — the same `WKWebView`
    /// and the same bridge that used to have a window to themselves. Held by
    /// value because the bridge is the web view's script-message handler and
    /// nothing else retains it.
    settings: SettingsPane,
    verbatim: Cell<bool>,
    /// What the sidebar was last reloaded for. Reloading twice a second would
    /// fight the user's scrolling for no gain.
    signature: RefCell<String>,
    /// What the transcript pane currently holds, so an unchanged session is
    /// not re-laid-out — and its scroll position not lost — on every tick.
    shown: RefCell<Option<(String, bool, SystemTime)>>,
    /// The phase's own failure text, projected once per `render` so that
    /// `repaint` — which handlers also call, and which has no phase to hand —
    /// stays the single writer of every control.
    phase_note: RefCell<Option<String>>,
    /// Set while the code, not the user, is moving the selection.
    quiet: Cell<bool>,
    painting: Cell<bool>,
    laid_out: Cell<(f64, f64, f64, f64, f64)>,
    /// The live recording, projected off the phase's shared `Meter` once per
    /// `render`. `None` between recordings, which is what removes the row.
    live: RefCell<Option<LiveShot>>,

    /// The naming strip's current rows, and what they were last built for —
    /// `(session id, detail stamp)`, the same key the transcript pane uses,
    /// so an append to `edits.jsonl` (naming a speaker) is what invalidates
    /// it and a timer tick alone does not.
    naming: RefCell<Vec<NamingRow>>,
    naming_shown: RefCell<Option<(String, SystemTime)>>,
    /// The session currently being diarized on a worker thread, and the
    /// channel its result arrives on. `Some` disables every Separate-voices
    /// button in the window, not just the one that was clicked — running two
    /// diarizers at once would double the ONNX memory for no reason.
    diarizing: RefCell<Option<(PathBuf, Receiver<anyhow::Result<usize>>)>>,
    /// Feedback from naming or diarizing that isn't a phase failure or a
    /// stored session warning — surfaced through the same banner, cleared
    /// whenever the selection changes.
    local_note: RefCell<Option<String>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AmbientSessionList"]
    #[ivars = Ivars]
    struct SessionList;

    unsafe impl NSObjectProtocol for SessionList {}
    /// Required: `NSTableViewDelegate` inherits from this, and every method of
    /// it is optional, so the empty impl is the whole of it.
    unsafe impl NSControlTextEditingDelegate for SessionList {}
    /// The window's delegate is in the responder chain, which is how the File
    /// menu's nil-targeted Copy Markdown and Reveal in Finder reach these
    /// methods.
    unsafe impl NSWindowDelegate for SessionList {
        /// Closing the window is the only thing that gives the Dock icon and
        /// the menu bar back. Minimising is not: the window is still there to
        /// be come back to, and an app that dropped out of Cmd-Tab when you
        /// minimised it would be unreachable.
        ///
        /// The window is ordered out **first**, so the demotion's own guard
        /// does not count the window it is closing as a reason to stay.
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, n: &NSNotification) {
            let mtm = MainThreadMarker::from(self);
            let window = n
                .object()
                .and_then(|o| o.downcast::<NSWindow>().ok());
            if let Some(w) = &window {
                w.orderOut(None);
            }
            demote_to_accessory(mtm, window.as_deref());
        }
    }

    unsafe impl NSTableViewDataSource for SessionList {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().rows.borrow().len() as NSInteger
        }
    }

    unsafe impl NSTableViewDelegate for SessionList {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            // One tail expression: inside a `method_id` body the macro rewrites
            // the return type, so neither `?` nor `return` compiles here. Both
            // are used freely in `cell_view`.
            self.cell_view(table, row)
        }

        /// Nothing is grouped yet. The hook exists because a flat table with
        /// this method is the whole of what an `NSOutlineView` was wanted for,
        /// at none of its cost.
        #[unsafe(method(tableView:isGroupRow:))]
        fn is_group_row(&self, _table: &NSTableView, _row: NSInteger) -> bool {
            false
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_did_change(&self, _n: &NSNotification) {
            if self.ivars().quiet.get() {
                return;
            }
            let mtm = MainThreadMarker::from(self);
            let row = self.ivars().views.table.selectedRow();
            // Scoped: `repaint` borrows `rows` mutably.
            let chosen = {
                let rows = self.ivars().rows.borrow();
                usize::try_from(row).ok().and_then(|r| rows.get(r)).map(|r| match r {
                    // The live row is a row like any other, and it is selected
                    // by the id of the directory the recording claimed — so
                    // when the recording ends and the row becomes an ordinary
                    // session, the selection is already on it.
                    Row::Session { id, .. } | Row::Live { id, .. } => {
                        Selection::Session(id.clone())
                    }
                    Row::Settings => Selection::Settings,
                })
            };
            if let Some(chosen) = chosen {
                if chosen == Selection::Settings {
                    // The CLI can have changed the file since the page last
                    // drew, exactly as it could when this was a window.
                    self.ivars().settings.refresh();
                }
                *self.ivars().selected.borrow_mut() = Some(chosen);
                *self.ivars().local_note.borrow_mut() = None;
                self.repaint(mtm);
            }
        }
    }

    impl SessionList {
        #[unsafe(method(toggleTranscriptMode:))]
        fn toggle_mode(&self, _sender: Option<&AnyObject>) {
            let mtm = MainThreadMarker::from(self);
            self.ivars()
                .verbatim
                .set(self.ivars().views.modes.selectedSegment() == 1);
            self.repaint(mtm);
        }

        /// The window's Stop. It does not stop anything itself: it forwards
        /// `stopRecording:` to whoever answers for it, which is the
        /// application delegate — the same object the status menu's Stop item
        /// targets, running the same transition. The window owns no piece of
        /// the recording's state, and a second way to end a capture would be
        /// a second source of truth about whether one is running.
        #[unsafe(method(stopLiveRecording:))]
        fn stop_live_recording(&self, _sender: Option<&NSButton>) {
            let mtm = MainThreadMarker::from(self);
            let app = NSApplication::sharedApplication(mtm);
            let me: &AnyObject = self;
            unsafe { app.sendAction_to_from(sel!(stopRecording:), None, Some(me)) };
        }

        #[unsafe(method(revealInFinder:))]
        fn reveal_in_finder(&self, _sender: Option<&AnyObject>) {
            // The sessions folder when nothing is selected: this item replaced
            // the status menu's Open Sessions Folder, and that had no
            // selection to depend on.
            let target = self
                .selected_dir()
                .unwrap_or_else(crate::session::home);
            std::fs::create_dir_all(&target).ok();
            let s = NSString::from_str(&target.to_string_lossy());
            if let Some(url) = NSURL::fileURLWithPath(&s).into() {
                let url: Retained<NSURL> = url;
                let urls = NSArray::from_slice(&[&*url]);
                NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&urls);
            }
        }

        #[unsafe(method(copyMarkdown:))]
        fn copy_markdown(&self, _sender: Option<&AnyObject>) {
            let Some(dir) = self.selected_dir() else {
                return;
            };
            // `transcript.md` already exists in the session directory, so an
            // NSSavePanel would be ceremony around a file the user can already
            // reach. Regenerated rather than read: the file is derived, and a
            // rename since it was written must not be lost here.
            let Ok(md) = session::markdown(&dir) else {
                return;
            };
            let pb = NSPasteboard::generalPasteboard();
            pb.clearContents();
            unsafe {
                pb.setString_forType(&NSString::from_str(&md), NSPasteboardTypeString);
            }
        }

        /// One naming-strip row's Name button. The row is found by `tag`,
        /// which is set to its index in `ivars().naming` when the row is
        /// built — row index, not session-row index, so step 7's live row
        /// pinning at sidebar index 0 has no bearing on it.
        #[unsafe(method(nameSpeaker:))]
        fn name_speaker_clicked(&self, sender: Option<&NSButton>) {
            let mtm = MainThreadMarker::from(self);
            let Some(sender) = sender else { return };
            let Some(dir) = self.selected_dir() else { return };
            let found = {
                let rows = self.ivars().naming.borrow();
                usize::try_from(sender.tag())
                    .ok()
                    .and_then(|i| rows.get(i))
                    .map(|r| (r.label.clone(), r.combo.stringValue().to_string()))
            };
            let Some((label, name)) = found else { return };
            let name = name.trim().to_string();
            if name.is_empty() {
                return;
            }
            match session::name_speaker(&dir, &label, &name) {
                Ok(_) => {
                    let mut roster = crate::roster::load();
                    if crate::roster::add(&mut roster, &name) {
                        crate::roster::save(&roster).ok();
                    }
                }
                Err(e) => {
                    *self.ivars().local_note.borrow_mut() = Some(format!("Could not name {label}: {e:#}"));
                }
            }
            // `name_speaker` appended to `edits.jsonl`, which moves this
            // session's `stamp_of` — the transcript, the sidebar subtitle and
            // this row set all notice on the next repaint without being told
            // which one changed.
            self.repaint(mtm);
        }

        /// Runs `diarize_session` on a worker thread — it loads two ONNX
        /// models and would beachball the window for tens of seconds
        /// otherwise. The button is disabled the moment this fires; `poll_diarize`
        /// (called from every `repaint`) picks up the result.
        #[unsafe(method(separateVoices:))]
        fn separate_voices_clicked(&self, _sender: Option<&NSButton>) {
            let mtm = MainThreadMarker::from(self);
            let Some(dir) = self.selected_dir() else { return };
            if self.ivars().diarizing.borrow().is_some() {
                return;
            }
            let threshold = crate::config::Config::load().threshold;
            let (tx, rx) = std::sync::mpsc::channel();
            let worker_dir = dir.clone();
            std::thread::spawn(move || {
                tx.send(crate::session::diarize_session(&worker_dir, threshold)).ok();
            });
            *self.ivars().diarizing.borrow_mut() = Some((dir, rx));
            self.repaint(mtm);
        }
    }
);

impl SessionList {
    /// The directory behind the selection — the live one included, so Reveal
    /// in Finder works on a recording in flight. Copy Markdown on one fails
    /// the way it already does for a session with no transcript.
    fn selected_dir(&self) -> Option<PathBuf> {
        let selected = self.ivars().selected.borrow().clone()?;
        let want = selected.session()?.to_string();
        self.ivars().rows.borrow().iter().find_map(|r| match r {
            Row::Session { dir, id, .. } | Row::Live { dir, id } if *id == want => {
                Some(dir.clone())
            }
            _ => None,
        })
    }

    /// The cell view for one row, reusing the one the table hands back when it
    /// has one. Recycling is the part of a view-based table that most often
    /// exposes an ownership bug, and the spike never filled its scroll view.
    fn cell_view(&self, table: &NSTableView, row: NSInteger) -> Option<Retained<NSView>> {
        let mtm = MainThreadMarker::from(self);
        let ident = NSString::from_str(CELL);
        let text = {
            let rows = self.ivars().rows.borrow();
            let row = rows.get(usize::try_from(row).ok()?)?;
            match row {
                Row::Session { detail, .. } => row_text(detail),
                // Drawn from the meter each time the row is asked for, which
                // is why the row itself carries no text: the clock moves every
                // second and the row *set* does not.
                Row::Live { .. } => match self.ivars().live.borrow().as_ref() {
                    Some(l) => live_row_text(l),
                    None => return None,
                },
                Row::Settings => {
                    let d = Detail {
                        title: "Settings".into(),
                        subtitle: "Watched apps, devices, retention".into(),
                        interrupted: false,
                        queued: false,
                        completed: false,
                        warnings: Vec::new(),
                        has_transcript: false,
                        stamp: SystemTime::UNIX_EPOCH,
                    };
                    row_text(&d)
                }
            }
        };
        let me: &AnyObject = self;
        let recycled = unsafe { table.makeViewWithIdentifier_owner(&ident, Some(me)) }
            .and_then(|v| v.downcast::<NSTextField>().ok());
        let field = match recycled {
            Some(f) => {
                f.setAttributedStringValue(&text);
                f
            }
            None => {
                let f = NSTextField::labelWithAttributedString(&text, mtm);
                f.setIdentifier(Some(&ident));
                f.setMaximumNumberOfLines(2);
                f.setSelectable(false);
                f
            }
        };
        // NSTextField -> NSControl -> NSView. The table retains what it
        // installs, so handing ours over is the whole ownership story.
        Some(Retained::into_super(Retained::into_super(field)))
    }

    /// The phase entry point. Projects the one thing the window needs from the
    /// phase and then hands off to [`SessionList::repaint`], which is the only
    /// writer of any control here.
    fn render(&self, phase: &Phase, mtm: MainThreadMarker) {
        *self.ivars().phase_note.borrow_mut() = phase.failure().map(str::to_string);
        // Read the atomics here, once, and treat them as a value from this
        // point on: everything below is a repaint that handlers also call, and
        // they have no phase to hand.
        let kind = phase.kind();
        *self.ivars().live.borrow_mut() = phase.live().map(|l| LiveShot::of(l, kind));
        // The settings bridge refuses to move the sessions folder while a
        // recording is held, and this is where it is told. Set here rather
        // than read there, so the pane keeps no state of its own about what
        // the app is doing.
        self.ivars().settings.set_recording(phase.live().is_some());
        self.repaint(mtm);
    }

    /// The marker is the caller's proof that this is the main thread, which
    /// every AppKit call below it assumes; nothing here needs to spell it.
    fn repaint(&self, _mtm: MainThreadMarker) {
        // A programmatic re-selection inside a reload calls straight back in.
        if self.ivars().painting.replace(true) {
            return;
        }
        self.poll_diarize();
        self.refresh_rows();
        self.refresh_pane();
        self.ivars().painting.set(false);
    }

    /// Notice when the diarize worker has finished. Called from every
    /// `repaint`, the same way the recording worker's receiver is polled from
    /// `menubar.rs`'s timer tick — a channel, not a flag file.
    fn poll_diarize(&self) {
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
        self.ivars().diarizing.borrow_mut().take();
        *self.ivars().local_note.borrow_mut() = Some(match result {
            Ok(n) => format!("Separated voices: {n} line(s) labelled."),
            Err(e) => format!("Separating voices failed: {e:#}"),
        });
    }

    /// Rebuild the sidebar's model, and reload only when it actually changed.
    fn refresh_rows(&self) {
        let home = session::home();
        let mut rows: Vec<Row> = Vec::new();
        let mut signature = String::new();
        let mut cache = self.ivars().cache.borrow_mut();
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let live = self.ivars().live.borrow().clone();

        // Pinned at index 0, which is why nothing below may be re-selected by
        // row index: every other row moves down one on entering Recording and
        // back up one on leaving it. Only the live *id* goes into the
        // signature — the clock inside the row changes every second and must
        // not be mistaken for a change to the row set.
        if let Some(l) = &live {
            signature.push_str(&l.id);
            signature.push('\u{4}');
            rows.push(Row::Live {
                dir: l.dir.clone(),
                id: l.id.clone(),
            });
        }

        // `session::list` is the one enumeration of the sessions folder, and it
        // carries the `is_dir` filter that keeps `app.log` from becoming a row.
        // Newest first, which is the order a person wants and the reverse of
        // the byte sort `latest()` needs.
        for dir in session::list(&home).into_iter().rev() {
            // The live recording claimed its directory before the worker
            // spawned, so it is already on disk and `list` already returns it.
            // Drawn once, as the live row, or the same id would name two rows
            // and re-selection would pick whichever came first.
            if live.as_ref().is_some_and(|l| l.dir == dir) {
                continue;
            }
            let fresh = stamp_of(&dir);
            let detail = match cache.get(&dir) {
                Some(d) if d.stamp == fresh => d.clone(),
                _ => {
                    let d = Rc::new(describe(&dir));
                    cache.insert(dir.clone(), d.clone());
                    d
                }
            };
            let id = dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            signature.push_str(&id);
            signature.push('\u{1}');
            signature.push_str(&detail.title);
            signature.push('\u{1}');
            signature.push_str(&detail.subtitle);
            signature.push('\u{2}');
            seen.insert(dir.clone());
            rows.push(Row::Session { dir, id, detail });
        }
        cache.retain(|k, _| seen.contains(k));
        drop(cache);
        rows.push(Row::Settings);

        *self.ivars().rows.borrow_mut() = rows;

        // Opening the window mid-recording with nothing ever selected should
        // land on the recording. Only ever fires while the selection is
        // genuinely unset — a selected session stays selected when a recording
        // starts underneath it, which is the whole point of keying on ids.
        if self.ivars().selected.borrow().is_none() {
            if let Some(l) = &live {
                *self.ivars().selected.borrow_mut() = Some(Selection::Session(l.id.clone()));
            }
        }

        if *self.ivars().signature.borrow() != signature {
            *self.ivars().signature.borrow_mut() = signature;
            self.ivars().views.table.reloadData();
            // `reloadData` cleared the selection and said nothing about it —
            // `tableViewSelectionDidChange:` does not fire for that clear.
            self.reselect();
        } else if live.is_some() {
            // The row set is unchanged but the live row's clock is not.
            // Reloading that one row leaves the selection alone; a full
            // `reloadData` twice a second would drop it and fight scrolling.
            let zero = NSIndexSet::indexSetWithIndex(0);
            self.ivars()
                .views
                .table
                .reloadDataForRowIndexes_columnIndexes(&zero, &zero);
        }
    }

    /// Put the selection back where the model says it is. By id, never by row.
    fn reselect(&self) {
        let want = self.ivars().selected.borrow().clone();
        let at = want.and_then(|want| {
            self.ivars()
                .rows
                .borrow()
                .iter()
                .position(|r| match (r, &want) {
                    (Row::Session { id, .. } | Row::Live { id, .. }, Selection::Session(w)) => {
                        id == w
                    }
                    (Row::Settings, Selection::Settings) => true,
                    _ => false,
                })
        });
        let table = &self.ivars().views.table;
        self.ivars().quiet.set(true);
        match at {
            Some(i) => {
                table.selectRowIndexes_byExtendingSelection(
                    &NSIndexSet::indexSetWithIndex(i as NSUInteger),
                    false,
                );
            }
            None => {
                table.selectRowIndexes_byExtendingSelection(&NSIndexSet::new(), false);
            }
        }
        self.ivars().quiet.set(false);
    }

    /// The single writer of every control on the right-hand side.
    fn refresh_pane(&self) {
        let v = &self.ivars().views;
        let selected = self.ivars().selected.borrow().clone();
        let settings_selected = selected.as_ref() == Some(&Selection::Settings);
        let want = selected.and_then(|s| s.session().map(str::to_string));
        let live = self.ivars().live.borrow().clone();
        // Selecting the live row shows the live view instead of a transcript:
        // there is no transcript yet, and the row it would look up is not a
        // `Row::Session`.
        let showing_live = live
            .as_ref()
            .filter(|l| want.as_deref() == Some(l.id.as_str()));
        let live_selected = showing_live.is_some();
        let chosen: Option<(PathBuf, String, Rc<Detail>)> = if live_selected {
            None
        } else {
            want.and_then(|want| {
                self.ivars().rows.borrow().iter().find_map(|r| match r {
                    Row::Session { dir, id, detail } if *id == want => {
                        Some((dir.clone(), id.clone(), detail.clone()))
                    }
                    _ => None,
                })
            })
        };

        // The siblings, toggled and never rebuilt. The transcript stack, the
        // live view and the settings page are three views of the one pane and
        // exactly one of them is up at a time.
        v.live.setHidden(!live_selected);
        v.bar.setHidden(live_selected || settings_selected);
        v.scroll.setHidden(live_selected || settings_selected);
        self.ivars().settings.view().setHidden(!settings_selected);
        if let Some(l) = showing_live {
            self.refresh_live(l);
        }

        let verbatim = self.ivars().verbatim.get();
        v.heading.setStringValue(&NSString::from_str(
            chosen
                .as_ref()
                .map(|(_, _, d)| d.title.as_str())
                .unwrap_or("No session selected"),
        ));
        v.modes.setSelected_forSegment(true, verbatim as NSInteger);
        v.modes.setEnabled(chosen.is_some());
        v.copy.setEnabled(chosen.is_some());
        // Reveal always works: with nothing selected it opens the sessions
        // folder, which is the item the status menu used to carry.
        v.reveal.setEnabled(true);

        // The banner. A live failure outranks naming/diarize feedback, which
        // outranks a stored warning — each is newer news than the last, but a
        // stored capture warning is why this strip exists at all.
        let note = self
            .ivars()
            .phase_note
            .borrow()
            .clone()
            .or_else(|| self.ivars().local_note.borrow().clone())
            .or_else(|| {
                chosen
                    .as_ref()
                    .filter(|(_, _, d)| !d.warnings.is_empty())
                    .map(|(_, _, d)| d.warnings.join("\n"))
            });
        match &note {
            Some(text) => {
                v.banner.setStringValue(&NSString::from_str(text));
                v.banner.setHidden(false);
            }
            None => {
                v.banner.setStringValue(ns_string!(""));
                v.banner.setHidden(true);
            }
        }

        // The transcript is expensive to build and holds a scroll position, so
        // it is rebuilt only when the session, the mode, or the session's own
        // files have changed.
        let key = chosen
            .as_ref()
            .map(|(_, id, d)| (id.clone(), verbatim, d.stamp));
        if *self.ivars().shown.borrow() != key {
            let body = match &chosen {
                Some((dir, _, d)) => transcript_text(dir, d, verbatim),
                None => run(
                    "Select a session on the left.",
                    &NSFont::systemFontOfSize(13.0),
                    &NSColor::secondaryLabelColor(),
                    false,
                ),
            };
            if let Some(store) = unsafe { v.text.textStorage() } {
                store.setAttributedString(&body);
            }
            *self.ivars().shown.borrow_mut() = key;
        }

        self.refresh_naming(&chosen);
        self.refresh_separate(&chosen);
        self.layout();
    }

    /// The live pane. Every value here came off the shared `Meter` in
    /// `render`; nothing in this function reads a file, and the elapsed clock
    /// is the worker's own count of the capture rather than wall time since
    /// the button was pressed — so it stops when the capture does.
    fn refresh_live(&self, live: &LiveShot) {
        let v = &self.ivars().views;
        v.live_elapsed
            .setStringValue(&NSString::from_str(&live.clock()));
        v.live_caption
            .setStringValue(&NSString::from_str(&live.capturing_line()));

        let capturing = live.meter == MeterPhase::Capturing;
        // Clamped rather than trusted: the peaks are amplitudes, but the
        // indicator's range is the contract this side of it.
        v.live_room.setDoubleValue(live.room.clamp(0.0, 1.0));
        v.live_call.setDoubleValue(live.call.clamp(0.0, 1.0));
        v.live_room.setEnabled(capturing);
        v.live_call.setEnabled(capturing);

        // The warning that matters: a tap that was denied returns
        // correctly-shaped zeros rather than an error, so "nothing is
        // arriving" is the only thing that distinguishes it from a quiet room
        // while the recording is still running.
        let warn = capturing && !live.audio_arriving;
        v.live_warning.setHidden(!warn);
        if warn {
            v.live_warning.setStringValue(&NSString::from_str(
                "No audio is arriving. Check the microphone permission and the \
                 input device in Settings — a capture that hears nothing still \
                 finishes, and still writes a session.",
            ));
        }

        v.live_stop.setEnabled(live.stoppable);
        // Not a button any more once the phase has left `Recording`: the
        // worker has moved on, and the only honest thing to show is what it
        // is doing. Enablement follows the phase, the label follows the
        // meter — the same split the menu's Stop item and status line use.
        let title = if live.stoppable {
            "Stop Recording".to_string()
        } else {
            format!("{}…", live.verb())
        };
        v.live_stop.setTitle(&NSString::from_str(&title));
    }

    /// Rebuild the naming strip, but only when the selected session or its
    /// `unnamed_labels` have actually changed — the same `(id, stamp)` key
    /// `refresh_pane` uses for the transcript, so a timer tick that finds
    /// nothing new does not wipe out a name someone is mid-typing into a
    /// combo box.
    fn refresh_naming(&self, chosen: &Option<(PathBuf, String, Rc<Detail>)>) {
        let key = chosen.as_ref().map(|(_, id, d)| (id.clone(), d.stamp));
        if *self.ivars().naming_shown.borrow() == key {
            return;
        }
        *self.ivars().naming_shown.borrow_mut() = key;
        // Past this point the row views are destroyed and rebuilt, and the new
        // ones start at the zero frame `NSComboBox::initWithFrame` was handed.
        // `layout`'s memo keys on the row *count*, so two sessions with the
        // same number of unnamed labels look identical to it and it returns
        // early — leaving invisible 0x0 combo boxes and Name buttons piled on
        // the Separate-voices button. Rebuilding rows is exactly the event the
        // memo cannot see, so invalidate it here rather than widening the key.
        self.ivars().laid_out.set((-1.0, -1.0, -1.0, -1.0, -1.0));

        for row in self.ivars().naming.borrow_mut().drain(..) {
            row.field.removeFromSuperview();
            row.combo.removeFromSuperview();
            row.button.removeFromSuperview();
        }

        let Some((dir, _, _)) = chosen else { return };
        let mtm = MainThreadMarker::from(self);
        let labels = match session::unnamed_labels(dir) {
            Ok(l) => l,
            Err(e) => {
                *self.ivars().local_note.borrow_mut() =
                    Some(format!("Could not read speaker labels: {e:#}"));
                Vec::new()
            }
        };
        let roster: Vec<Retained<NSString>> = crate::roster::load()
            .iter()
            .map(|s| NSString::from_str(s))
            .collect();
        let roster_refs: Vec<&AnyObject> = roster.iter().map(|s| -> &AnyObject { s }).collect();
        let roster_items = NSArray::from_slice(&roster_refs);
        let me: &AnyObject = self;
        let pane: &NSView = &self.ivars().views.pane;

        let mut rows = Vec::new();
        for (i, (label, sample)) in labels.into_iter().enumerate() {
            let text = run(
                &format!("{label} — {sample}"),
                &NSFont::systemFontOfSize(12.0),
                &NSColor::secondaryLabelColor(),
                true,
            );
            let field = NSTextField::labelWithAttributedString(&text, mtm);
            field.setMaximumNumberOfLines(1);

            let combo = NSComboBox::initWithFrame(NSComboBox::alloc(mtm), NSRect::ZERO);
            unsafe { combo.addItemsWithObjectValues(&roster_items) };
            combo.setPlaceholderString(Some(ns_string!("Name")));
            combo.setCompletes(true);

            let button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    ns_string!("Name"),
                    Some(me),
                    Some(sel!(nameSpeaker:)),
                    mtm,
                )
            };
            button.setBezelStyle(NSBezelStyle::Push);
            button.setTag(i as NSInteger);

            pane.addSubview(&field);
            pane.addSubview(&combo);
            pane.addSubview(&button);
            rows.push(NamingRow {
                field,
                combo,
                button,
                label,
            });
        }
        *self.ivars().naming.borrow_mut() = rows;
    }

    /// The Separate-voices button and its explanation. Unlike the naming
    /// strip this has no rebuild-only-on-change gate: it is two `exists()`
    /// checks and some string comparisons, run once a tick, and it has to
    /// react immediately to a worker finishing or starting.
    fn refresh_separate(&self, chosen: &Option<(PathBuf, String, Rc<Detail>)>) {
        let v = &self.ivars().views;
        v.separate.setHidden(chosen.is_none());
        v.separate_note.setHidden(true);
        let Some((dir, _, detail)) = chosen else {
            return;
        };

        let diarizing = self.ivars().diarizing.borrow();
        let diarizing_this = diarizing.as_ref().is_some_and(|(d, _)| d == dir);
        let diarizing_other = diarizing.as_ref().is_some_and(|(d, _)| d != dir);
        drop(diarizing);

        let audio = dir.join("audio");
        let swept = !audio.join("room.wav").exists() && !audio.join("call.wav").exists();

        let (enabled, note) = if diarizing_this {
            (false, String::new())
        } else if diarizing_other {
            (
                false,
                "Another session is being processed right now.".to_string(),
            )
        } else if swept {
            (
                false,
                "The audio for this session has already been swept by retention — \
                 there is nothing left to separate voices from."
                    .to_string(),
            )
        } else if !detail.has_transcript {
            (
                false,
                "No transcript to attribute speakers to yet.".to_string(),
            )
        } else {
            (true, String::new())
        };

        v.separate.setEnabled(enabled);
        v.separate.setTitle(if diarizing_this {
            ns_string!("Separating voices…")
        } else {
            ns_string!("Separate voices")
        });
        if !note.is_empty() {
            v.separate_note.setStringValue(&NSString::from_str(&note));
            v.separate_note.setHidden(false);
        }
    }

    /// Frames, not Auto Layout — the same shape the rest of this codebase
    /// uses. Autoresizing masks carry a live resize; this corrects it on the
    /// next tick and is what moves the transcript when the banner appears.
    fn layout(&self) {
        let v = &self.ivars().views;
        let b = v.pane.bounds();
        let banner_h = if v.banner.isHidden() {
            0.0
        } else {
            BANNER_HEIGHT
        };
        // The naming strip and the Separate-voices row below it, together the
        // footer this step adds under the transcript. Both are hidden when
        // nothing is selected, in which case the footer takes no room at all
        // and the transcript fills the pane exactly as it did before step 6.
        let rows = self.ivars().naming.borrow().len();
        let footer_h = if v.separate.isHidden() {
            0.0
        } else {
            let naming_h = if rows > 0 {
                rows as f64 * NAMING_ROW_HEIGHT + INSET / 2.0
            } else {
                0.0
            };
            FOOTER_HEIGHT + naming_h + INSET / 2.0
        };
        // The live view's own children are laid out below, so whether it is
        // showing has to be part of the memo — a pane that swapped from the
        // transcript to the live view at an unchanged size would otherwise
        // keep the zero frames its subviews were created with.
        let live_h = if v.live.isHidden() { 0.0 } else { 1.0 };
        let want = (b.size.width, b.size.height, banner_h, footer_h, live_h);
        if self.ivars().laid_out.get() == want {
            return;
        }
        self.ivars().laid_out.set(want);

        let (w, h) = (b.size.width, b.size.height);
        v.banner.setFrame(NSRect::new(
            NSPoint::new(0.0, h - banner_h),
            NSSize::new(w, banner_h),
        ));
        let bar_y = h - banner_h - BAR_HEIGHT - INSET / 2.0;
        v.bar.setFrame(NSRect::new(
            NSPoint::new(0.0, bar_y),
            NSSize::new(w, BAR_HEIGHT),
        ));
        v.scroll.setFrame(NSRect::new(
            NSPoint::new(0.0, footer_h),
            NSSize::new(w, (bar_y - footer_h).max(0.0)),
        ));

        // The footer, bottom to top: Separate voices at y = 0, then the
        // naming rows stacked above it — the same order the design puts them
        // in, read top to bottom as transcript / naming strip / Separate
        // voices.
        v.separate.sizeToFit();
        let sep_w = v.separate.frame().size.width;
        v.separate.setFrame(NSRect::new(
            NSPoint::new(INSET, 4.0),
            NSSize::new(sep_w, BAR_HEIGHT - 8.0),
        ));
        v.separate_note.setFrame(NSRect::new(
            NSPoint::new(INSET * 2.0 + sep_w, 0.0),
            NSSize::new((w - INSET * 3.0 - sep_w).max(0.0), FOOTER_HEIGHT),
        ));
        for (i, row) in self.ivars().naming.borrow().iter().enumerate() {
            let y = FOOTER_HEIGHT + INSET / 2.0 + i as f64 * NAMING_ROW_HEIGHT;
            let button_w = 60.0;
            let combo_w = 160.0;
            row.button.setFrame(NSRect::new(
                NSPoint::new(w - INSET - button_w, y + 2.0),
                NSSize::new(button_w, NAMING_ROW_HEIGHT - 6.0),
            ));
            row.combo.setFrame(NSRect::new(
                NSPoint::new(w - INSET - button_w - 8.0 - combo_w, y + 2.0),
                NSSize::new(combo_w, NAMING_ROW_HEIGHT - 6.0),
            ));
            let field_w = (w - INSET * 3.0 - button_w - 8.0 - combo_w - 8.0).max(0.0);
            row.field.setFrame(NSRect::new(
                NSPoint::new(INSET, y + 4.0),
                NSSize::new(field_w, NAMING_ROW_HEIGHT - 8.0),
            ));
        }

        // The settings page fills everything under the banner. It has no
        // children of this file's to place — the page lays itself out — so it
        // is only ever framed, never inspected.
        self.ivars().settings.view().setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(w, (h - banner_h).max(0.0)),
        ));

        // The live view fills everything under the banner, and stacks its own
        // children downward from the top of that.
        let live_height = (h - banner_h).max(0.0);
        v.live.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(w, live_height),
        ));
        {
            let inner = w - INSET * 4.0;
            let mut y = live_height - 56.0;
            v.live_elapsed.setFrame(NSRect::new(
                NSPoint::new(INSET * 2.0, y),
                NSSize::new(inner.max(0.0), 46.0),
            ));
            y -= 30.0;
            v.live_caption.setFrame(NSRect::new(
                NSPoint::new(INSET * 2.0, y),
                NSSize::new(inner.max(0.0), 20.0),
            ));

            // Two meters, room over call, each with its caption to the left so
            // the pair reads as one block rather than two controls.
            let label_w = 96.0;
            let meter_w = (inner - label_w - 10.0).max(0.0);
            for (i, (label, meter)) in [
                (&v.live_room_label, &v.live_room),
                (&v.live_call_label, &v.live_call),
            ]
            .into_iter()
            .enumerate()
            {
                let row_y = y - 44.0 - i as f64 * 30.0;
                label.setFrame(NSRect::new(
                    NSPoint::new(INSET * 2.0, row_y),
                    NSSize::new(label_w, 20.0),
                ));
                meter.setFrame(NSRect::new(
                    NSPoint::new(INSET * 2.0 + label_w + 10.0, row_y + 2.0),
                    NSSize::new(meter_w, 16.0),
                ));
            }
            y -= 44.0 + 30.0;

            y -= 56.0;
            v.live_warning.setFrame(NSRect::new(
                NSPoint::new(INSET * 2.0, y),
                NSSize::new(inner.max(0.0), 44.0),
            ));

            v.live_stop.sizeToFit();
            let stop_w = v.live_stop.frame().size.width.max(140.0);
            v.live_stop.setFrame(NSRect::new(
                NSPoint::new(INSET * 2.0, (y - 46.0).max(INSET)),
                NSSize::new(stop_w, 30.0),
            ));
        }

        // Inside the bar, right to left.
        let mut right = w - INSET;
        for c in [&v.copy, &v.reveal] {
            let cw = c.frame().size.width;
            c.setFrame(NSRect::new(
                NSPoint::new(right - cw, 0.0),
                NSSize::new(cw, BAR_HEIGHT),
            ));
            right -= cw + 8.0;
        }
        let mw = v.modes.frame().size.width;
        v.modes.setFrame(NSRect::new(
            NSPoint::new(right - mw, 2.0),
            NSSize::new(mw, BAR_HEIGHT - 4.0),
        ));
        right -= mw + 12.0;
        v.heading.setFrame(NSRect::new(
            NSPoint::new(INSET, 4.0),
            NSSize::new((right - INSET).max(0.0), BAR_HEIGHT - 8.0),
        ));
    }
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

/// Held by the app delegate across opens: closing must not deallocate it,
/// because the menu reopens this one — and because the settings bridge lives
/// inside it now and nothing else retains that.
pub struct MainWindow {
    window: Retained<NSWindow>,
    list: Retained<SessionList>,
}

impl MainWindow {
    pub fn open(mtm: MainThreadMarker) -> MainWindow {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(960.0, 620.0));
        let window: Retained<NSWindow> = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(ns_string!("Ambient"));
        unsafe {
            window.setReleasedWhenClosed(false);
            window.setContentMinSize(NSSize::new(700.0, 440.0));
        }
        window.setFrameAutosaveName(ns_string!("AmbientMain"));

        let content_h = frame.size.height;
        let pane_w = frame.size.width - SIDEBAR_WIDTH;

        // --- sidebar ---------------------------------------------------
        let table = NSTableView::initWithFrame(
            NSTableView::alloc(mtm),
            NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(SIDEBAR_WIDTH, content_h),
            ),
        );
        let column =
            NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), ns_string!("session"));
        column.setWidth(SIDEBAR_WIDTH - 20.0);
        column.setMinWidth(120.0);
        table.addTableColumn(&column);
        table.setHeaderView(None);
        table.setRowHeight(ROW_HEIGHT);
        table.setAllowsEmptySelection(true);
        table.setStyle(NSTableViewStyle::SourceList);

        let sidebar = NSScrollView::initWithFrame(
            NSScrollView::alloc(mtm),
            NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(SIDEBAR_WIDTH, content_h),
            ),
        );
        sidebar.setHasVerticalScroller(true);
        sidebar.setAutohidesScrollers(true);
        sidebar.setDocumentView(Some(&table));

        // --- pane ------------------------------------------------------
        let pane = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(pane_w, content_h)),
        );

        let banner = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
        banner.setSelectable(true);
        banner.setDrawsBackground(true);
        banner.setBackgroundColor(Some(
            &NSColor::systemYellowColor().colorWithAlphaComponent(0.22),
        ));
        banner.setTextColor(Some(&NSColor::labelColor()));
        banner.setMaximumNumberOfLines(3);
        banner.setHidden(true);
        banner.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
        );

        let bar = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(pane_w, BAR_HEIGHT)),
        );
        bar.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
        );

        let heading = NSTextField::labelWithString(ns_string!("No session selected"), mtm);
        heading.setFont(Some(&NSFont::boldSystemFontOfSize(14.0)));
        heading.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);

        // The `verbatim` flag the CLI has had all along and no GUI has ever
        // surfaced.
        let labels = NSArray::from_retained_slice(&[
            NSString::from_str("Tidied"),
            NSString::from_str("Verbatim"),
        ]);
        let modes = unsafe {
            NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                &labels,
                NSSegmentSwitchTracking::SelectOne,
                None,
                Some(sel!(toggleTranscriptMode:)),
                mtm,
            )
        };
        modes.sizeToFit();
        modes.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin);

        let reveal = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Reveal in Finder"),
                None,
                Some(sel!(revealInFinder:)),
                mtm,
            )
        };
        let copy = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Copy Markdown"),
                None,
                Some(sel!(copyMarkdown:)),
                mtm,
            )
        };
        for b in [&reveal, &copy] {
            b.setBezelStyle(NSBezelStyle::Push);
            b.sizeToFit();
            b.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin);
        }

        bar.addSubview(&heading);
        bar.addSubview(&modes);
        bar.addSubview(&reveal);
        bar.addSubview(&copy);

        // `scrollableTextView` hands back a fully wired NSScrollView, which is
        // the whole reason the transcript is a text view and not a table: no
        // documentView, no textContainer, no layout manager to assemble.
        let scroll = NSTextView::scrollableTextView(mtm);
        scroll.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        let text = scroll
            .documentView()
            .and_then(|v| v.downcast::<NSTextView>().ok())
            .expect("scrollableTextView returns a scroll view around an NSTextView");
        text.setEditable(false);
        text.setSelectable(true);
        text.setRichText(true);
        text.setTextContainerInset(NSSize::new(14.0, 12.0));

        // The Separate-voices row, pinned at the very bottom of the pane.
        // Always created, hidden by `refresh_separate` until a session is
        // selected — the naming strip above it is rebuilt per session, but
        // this one control is not.
        let separate = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Separate voices"),
                None,
                Some(sel!(separateVoices:)),
                mtm,
            )
        };
        separate.setBezelStyle(NSBezelStyle::Push);
        separate.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMaxXMargin);
        let separate_note = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
        separate_note.setTextColor(Some(&NSColor::secondaryLabelColor()));
        separate_note.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        separate_note.setHidden(true);
        separate_note.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);

        // --- the live view --------------------------------------------
        // The fourth sibling: built once, hidden until the live row is the
        // selected one, and never rebuilt. Everything in it is written by
        // `refresh_live` from the shared `Meter`.
        let live = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(pane_w, content_h)),
        );
        live.setHidden(true);
        live.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );

        // Monospaced digits, or the clock jitters sideways every time a 1
        // becomes a 2 — at 34pt that is the most visible thing in the window.
        let clock_font =
            unsafe { NSFont::monospacedDigitSystemFontOfSize_weight(34.0, NSFontWeightMedium) };
        let live_elapsed = NSTextField::labelWithString(ns_string!("00:00"), mtm);
        live_elapsed.setFont(Some(&clock_font));
        live_elapsed.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);

        let live_caption = NSTextField::labelWithString(ns_string!(""), mtm);
        live_caption.setTextColor(Some(&NSColor::secondaryLabelColor()));
        live_caption.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);

        let live_room_label = NSTextField::labelWithString(ns_string!("Room (you)"), mtm);
        let live_call_label = NSTextField::labelWithString(ns_string!("Call (them)"), mtm);
        for l in [&live_room_label, &live_call_label] {
            l.setFont(Some(&NSFont::systemFontOfSize(12.0)));
            l.setTextColor(Some(&NSColor::secondaryLabelColor()));
        }

        let live_room = NSLevelIndicator::initWithFrame(NSLevelIndicator::alloc(mtm), NSRect::ZERO);
        let live_call = NSLevelIndicator::initWithFrame(NSLevelIndicator::alloc(mtm), NSRect::ZERO);
        for m in [&live_room, &live_call] {
            m.setLevelIndicatorStyle(NSLevelIndicatorStyle::ContinuousCapacity);
            m.setMinValue(0.0);
            m.setMaxValue(1.0);
            m.setWarningValue(0.75);
            m.setCriticalValue(0.95);
            m.setEditable(false);
            m.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        }

        let live_warning = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
        live_warning.setTextColor(Some(&NSColor::systemOrangeColor()));
        live_warning.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        live_warning.setHidden(true);
        live_warning.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);

        // Targets this window's delegate, which forwards `stopRecording:` up
        // the responder chain to the application delegate — the same object,
        // and the same method, the status menu's Stop item sends it to. One
        // code path, two views. Forwarded explicitly rather than left
        // nil-targeted because that is the dispatch this file has already
        // proven works, in the Settings row.
        let live_stop = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Stop Recording"),
                None,
                Some(sel!(stopLiveRecording:)),
                mtm,
            )
        };
        live_stop.setBezelStyle(NSBezelStyle::Push);
        live_stop.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMaxXMargin);

        live.addSubview(&live_elapsed);
        live.addSubview(&live_caption);
        live.addSubview(&live_room_label);
        live.addSubview(&live_call_label);
        live.addSubview(&live_room);
        live.addSubview(&live_call);
        live.addSubview(&live_warning);
        live.addSubview(&live_stop);

        // --- the settings view ----------------------------------------
        // The existing `WKWebView` and its bridge, re-parented verbatim: the
        // page, `Bridge::handle` and the roster/naming plumbing behind it are
        // untouched. One window means one menu bar to keep straight under the
        // activation-policy flip.
        let settings = SettingsPane::new(mtm);

        pane.addSubview(&scroll);
        pane.addSubview(&bar);
        pane.addSubview(&banner);
        pane.addSubview(&separate);
        pane.addSubview(&separate_note);
        pane.addSubview(&live);
        pane.addSubview(settings.view());

        // --- split -----------------------------------------------------
        let split = NSSplitView::initWithFrame(NSSplitView::alloc(mtm), frame);
        split.setVertical(true);
        split.setDividerStyle(NSSplitViewDividerStyle::Thin);
        split.setAutosaveName(Some(ns_string!("AmbientSplit")));
        split.addSubview(&sidebar);
        split.addSubview(&pane);
        split.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        window.setContentView(Some(&split));
        split.setPosition_ofDividerAtIndex(SIDEBAR_WIDTH, 0);
        window.center();

        let list = SessionList::alloc(mtm).set_ivars(Ivars {
            views: Views {
                table: table.clone(),
                pane,
                bar,
                heading,
                modes: modes.clone(),
                reveal: reveal.clone(),
                copy: copy.clone(),
                banner,
                scroll,
                text,
                separate: separate.clone(),
                separate_note,
                live,
                live_elapsed,
                live_caption,
                live_room_label,
                live_call_label,
                live_room,
                live_call,
                live_warning,
                live_stop: live_stop.clone(),
            },
            rows: RefCell::new(Vec::new()),
            cache: RefCell::new(HashMap::new()),
            selected: RefCell::new(None),
            settings,
            verbatim: Cell::new(false),
            signature: RefCell::new(String::from("\u{3}never rendered")),
            shown: RefCell::new(None),
            phase_note: RefCell::new(None),
            quiet: Cell::new(false),
            painting: Cell::new(false),
            laid_out: Cell::new((-1.0, -1.0, -1.0, -1.0, -1.0)),
            live: RefCell::new(None),
            naming: RefCell::new(Vec::new()),
            naming_shown: RefCell::new(None),
            diarizing: RefCell::new(None),
            local_note: RefCell::new(None),
        });
        let list: Retained<SessionList> = unsafe { msg_send![super(list), init] };

        let target: &AnyObject = &list;
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(&*list)));
            table.setDelegate(Some(ProtocolObject::from_ref(&*list)));
            window.setDelegate(Some(ProtocolObject::from_ref(&*list)));
            modes.setTarget(Some(target));
            reveal.setTarget(Some(target));
            copy.setTarget(Some(target));
            separate.setTarget(Some(target));
            live_stop.setTarget(Some(target));
        }

        MainWindow { window, list }
    }

    /// Repaint from the phase. Called from the delegate's existing timer, the
    /// one added in `NSRunLoopCommonModes` — a `scheduledTimer` stops during
    /// menu tracking and any modal panel, which is exactly when this window
    /// must keep moving.
    pub fn render(&self, phase: &Phase, mtm: MainThreadMarker) {
        // A closed window has nothing to show, and a refresh stats four files
        // per session — twice a second, for the whole of the app's life,
        // against a folder that only grows. The window is closed for almost
        // all of it.
        if !self.window.isVisible() {
            return;
        }
        self.list.render(phase, mtm);
    }

    /// Bring it forward, and promote the app to `Regular` while it is up.
    ///
    /// Order matters: the policy first, then the activation, then the window.
    /// `activateIgnoringOtherApps` under `Accessory` raises a window with no
    /// menu bar of its own, and a window whose whole job is reading and
    /// copying text needs ⌘C, ⌘A, ⌘W and ⌘Q to work.
    ///
    /// Called only from a user action — Open Ambient, Settings…, or the Dock
    /// icon's reopen. **Never** from a background event: a call starting must
    /// not put Ambient in front of the meeting the user is in.
    pub fn show(&self, mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        self.window.makeKeyAndOrderFront(None);
    }

    /// Put the sidebar on the Settings row, for the menu's Settings… item.
    /// The row is selected here rather than painted here: the pane follows the
    /// selection on the next `render`, which is the only writer of any control.
    pub fn select_settings(&self) {
        *self.list.ivars().selected.borrow_mut() = Some(Selection::Settings);
        self.list.ivars().settings.refresh();
        self.list.reselect();
    }

    pub fn is_visible(&self) -> bool {
        self.window.isVisible()
    }

    /// For the launch log: a window nobody can see is the failure worth being
    /// able to point at, and a bundle launch has nowhere to print it.
    ///
    /// The selection is reported as **both** the id and the row it currently
    /// occupies, because those two drifting apart is precisely the failure the
    /// live row can cause: pinning it at index 0 shifts every session below
    /// it, and `reloadData` drops the selection without firing
    /// `tableViewSelectionDidChange:`.
    pub fn describe_state(&self) -> String {
        let ivars = self.list.ivars();
        let v = &ivars.views;
        let selected = ivars.selected.borrow().clone();
        // Read back off the controls themselves rather than off the values
        // they were set from: what this is for is telling a rebuilt-and-run
        // check apart from a compiled one.
        let row0 = v
            .table
            .viewAtColumn_row_makeIfNecessary(0, 0, false)
            .and_then(|view| view.downcast::<NSTextField>().ok())
            .map(|f| f.stringValue().to_string().replace('\n', " / "))
            .unwrap_or_else(|| "<no view>".into());
        let pane = if v.live.isHidden() {
            "hidden".to_string()
        } else {
            format!(
                "showing {} room {:.2} call {:.2} [{}{}]{}",
                v.live_elapsed.stringValue(),
                v.live_room.doubleValue(),
                v.live_call.doubleValue(),
                v.live_stop.title(),
                if v.live_stop.isEnabled() {
                    ""
                } else {
                    ", disabled"
                },
                if v.live_warning.isHidden() {
                    ""
                } else {
                    " NO AUDIO"
                },
            )
        };
        format!(
            "window visible: {}, policy: {}, rows: {}, row 0: {row0:?}, live: {}, \
             selected: {} at row {}, live pane: {pane}, settings pane: {}",
            self.window.isVisible(),
            policy_name(MainThreadMarker::from(&*self.list)),
            v.table.numberOfRows(),
            ivars
                .live
                .borrow()
                .as_ref()
                .map(|l| l.id.clone())
                .unwrap_or_else(|| "none".into()),
            match &selected {
                Some(Selection::Session(id)) => id.as_str(),
                Some(Selection::Settings) => "<settings>",
                None => "none",
            },
            v.table.selectedRow(),
            if ivars.settings.view().isHidden() {
                format!("hidden, {}", ivars.settings.describe())
            } else {
                format!("shown, {}", ivars.settings.describe())
            },
        )
    }
}

// ---------------------------------------------------------------------------
// Activation policy
// ---------------------------------------------------------------------------

/// A visible window that is not `except` and is a window a person could be
/// looking at.
///
/// The filter is not cosmetic. `NSApp.windows()` carries AppKit's own
/// furniture — the status item's bar window, the carrier window behind an open
/// `NSMenu` — and every one of those is `isVisible`. Counting them would mean
/// the app never demoted at all. `canBecomeKeyWindow` is what separates them:
/// they are borderless and refuse key, while a panel, an alert and the
/// standard About window all accept it.
fn other_visible_window(
    app: &NSApplication,
    except: Option<&NSWindow>,
) -> Option<Retained<NSWindow>> {
    app.windows().iter().find(|w| {
        let same = except.is_some_and(|e| std::ptr::eq(&**w as *const NSWindow, e));
        !same && w.isVisible() && w.canBecomeKeyWindow()
    })
}

/// Give the Dock icon and the menu bar back, if nothing on screen still needs
/// them.
///
/// Two guards, both of which have to hold:
///
/// * **Another window is up.** Demoting takes the main menu away from whatever
///   is still open — the About panel, an alert — which reads as a broken app.
/// * **A modal session is running.** `NSOpenPanel::runModal`, which the
///   settings page's Choose… buttons use, spins its own run loop; changing the
///   activation policy underneath it is the standard way this pattern
///   deadlocks.
///
/// Nothing here is called on a background event. The only caller is
/// `windowWillClose:`, and minimising does not close.
pub fn demote_to_accessory(mtm: MainThreadMarker, closing: Option<&NSWindow>) {
    let app = NSApplication::sharedApplication(mtm);
    if app.activationPolicy() != NSApplicationActivationPolicy::Regular {
        return;
    }
    // A refusal is worth a log line when it explains a close that did not
    // demote, and pure noise on the retry tick, which asks the same question
    // twice a second for the whole time the window is open. `closing` is
    // exactly that distinction: `windowWillClose:` passes the window, the
    // retry passes None.
    let announce = closing.is_some();
    if app.modalWindow().is_some() {
        if announce {
            eprintln!("window: staying Regular — a modal panel is up");
        }
        return;
    }
    if let Some(w) = other_visible_window(&app, closing) {
        if announce {
            eprintln!("window: staying Regular — {} is still visible", w.title());
        }
        return;
    }
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    eprintln!("window: closed — back to Accessory");
}

/// What the activation policy is, for a log line a bundle launch can show.
pub fn policy_name(mtm: MainThreadMarker) -> &'static str {
    match NSApplication::sharedApplication(mtm).activationPolicy() {
        NSApplicationActivationPolicy::Regular => "Regular",
        NSApplicationActivationPolicy::Accessory => "Accessory",
        _ => "Prohibited",
    }
}

// ---------------------------------------------------------------------------
// The application main menu
// ---------------------------------------------------------------------------

fn menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<objc2::runtime::Sel>,
    key: &str,
    mask: Option<NSEventModifierFlags>,
) -> Retained<NSMenuItem> {
    let i = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    };
    if let Some(m) = mask {
        i.setKeyEquivalentModifierMask(m);
    }
    i
}

fn submenu(
    mtm: MainThreadMarker,
    title: &str,
    items: Vec<Retained<NSMenuItem>>,
) -> Retained<NSMenu> {
    let m = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    for i in items {
        m.addItem(&i);
    }
    m
}

/// Install a real application main menu.
///
/// Every item is **nil-targeted**, so each one is dispatched down the
/// responder chain: `openSettings:` reaches the app delegate, Copy Markdown
/// and Reveal in Finder reach the main window's delegate, and the Edit menu's
/// standard selectors reach whatever text is being edited — the transcript
/// view, the combo boxes step 6 adds, and the settings `WKWebView`.
///
/// Inert under Accessory, mandatory under Regular: promoting the activation
/// policy without this yields a menu bar holding only the Apple menu, which
/// reads as a broken app. That is why it is installed from this step, before
/// step 8 makes the flip.
pub fn install_main_menu(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let cmd_shift = NSEventModifierFlags::Command | NSEventModifierFlags::Shift;
    let cmd_opt = NSEventModifierFlags::Command | NSEventModifierFlags::Option;

    let ambient = submenu(
        mtm,
        "Ambient",
        vec![
            menu_item(
                mtm,
                "About Ambient",
                Some(sel!(orderFrontStandardAboutPanel:)),
                "",
                None,
            ),
            NSMenuItem::separatorItem(mtm),
            menu_item(mtm, "Settings…", Some(sel!(openSettings:)), ",", None),
            NSMenuItem::separatorItem(mtm),
            menu_item(mtm, "Hide Ambient", Some(sel!(hide:)), "h", None),
            menu_item(
                mtm,
                "Hide Others",
                Some(sel!(hideOtherApplications:)),
                "h",
                Some(cmd_opt),
            ),
            menu_item(
                mtm,
                "Show All",
                Some(sel!(unhideAllApplications:)),
                "",
                None,
            ),
            NSMenuItem::separatorItem(mtm),
            menu_item(mtm, "Quit Ambient", Some(sel!(terminate:)), "q", None),
        ],
    );

    let edit = submenu(
        mtm,
        "Edit",
        vec![
            menu_item(mtm, "Undo", Some(sel!(undo:)), "z", None),
            menu_item(mtm, "Redo", Some(sel!(redo:)), "z", Some(cmd_shift)),
            NSMenuItem::separatorItem(mtm),
            menu_item(mtm, "Cut", Some(sel!(cut:)), "x", None),
            menu_item(mtm, "Copy", Some(sel!(copy:)), "c", None),
            menu_item(mtm, "Paste", Some(sel!(paste:)), "v", None),
            NSMenuItem::separatorItem(mtm),
            menu_item(mtm, "Select All", Some(sel!(selectAll:)), "a", None),
        ],
    );

    let file = submenu(
        mtm,
        "File",
        vec![
            menu_item(
                mtm,
                "Copy Markdown",
                Some(sel!(copyMarkdown:)),
                "c",
                Some(cmd_shift),
            ),
            menu_item(
                mtm,
                "Reveal in Finder",
                Some(sel!(revealInFinder:)),
                "r",
                Some(cmd_shift),
            ),
            NSMenuItem::separatorItem(mtm),
            menu_item(mtm, "Close", Some(sel!(performClose:)), "w", None),
        ],
    );

    let windows = submenu(
        mtm,
        "Window",
        vec![
            menu_item(mtm, "Minimize", Some(sel!(performMiniaturize:)), "m", None),
            menu_item(mtm, "Zoom", Some(sel!(performZoom:)), "", None),
            NSMenuItem::separatorItem(mtm),
            menu_item(
                mtm,
                "Bring All to Front",
                Some(sel!(arrangeInFront:)),
                "",
                None,
            ),
        ],
    );

    let main = NSMenu::new(mtm);
    for m in [&ambient, &edit, &file, &windows] {
        let holder = menu_item(mtm, &m.title().to_string(), None, "", None);
        holder.setSubmenu(Some(m));
        main.addItem(&holder);
    }
    app.setMainMenu(Some(&main));
    app.setWindowsMenu(Some(&windows));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ambient-window-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&p).ok();
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// The row's second line is the whole of the sidebar's information, and
    /// every part of it is derived rather than stored.
    #[test]
    fn a_transcribed_session_says_when_how_long_and_who() {
        let root = scratch("described");
        let dir = root.join("2026-08-30T1412");
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        std::fs::write(
            dir.join("raw.jsonl"),
            "{\"track\":\"room\",\"start_ms\":0,\"end_ms\":2000,\"text\":\"hello\",\"confidence\":0.9}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("edits.jsonl"),
            "{\"kind\":\"speaker\",\"target\":{\"track\":\"room\",\"start_ms\":0},\"name\":\"room-1\",\"by\":\"diarize\",\"at\":\"2026-08-30T14:13:00+10:00\"}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("session.json"),
            "{\"id\":\"2026-08-30T1412\",\"name\":null,\"started_at\":\"2026-08-30T14:12:00+10:00\",\"ended_at\":\"2026-08-30T14:13:05+10:00\",\"duration_s\":65.0,\"device_hz\":48000,\"channels\":1,\"mic_channels\":1,\"apps\":[],\"model\":\"m\",\"warnings\":[\"the room track is silent\"]}",
        )
        .unwrap();

        let d = describe(&dir);
        assert_eq!(d.title, "2026-08-30T1412");
        assert!(d.has_transcript);
        assert!(!d.interrupted);
        assert!(d.subtitle.contains("01:05"), "duration: {}", d.subtitle);
        assert!(d.subtitle.contains("1 speaker"), "speakers: {}", d.subtitle);
        // `room-1` still has diarization's shape, so it is a label waiting for
        // a name — the badge the whole naming step hangs off.
        assert!(d.subtitle.contains("1 to name"), "unnamed: {}", d.subtitle);
        assert_eq!(d.warnings, vec!["the room track is silent".to_string()]);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Audio, no transcript, nothing writing: a process killed mid-recording.
    #[test]
    fn audio_without_a_transcript_reads_as_interrupted() {
        let root = scratch("interrupted");
        let dir = root.join("2026-08-30T1500");
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        std::fs::write(dir.join("audio").join("room.native.wav"), b"RIFF").unwrap();
        // Backdate it past the ten-second window `is_growing` allows.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        filetime_set(&dir.join("audio").join("room.native.wav"), old);

        let d = describe(&dir);
        assert!(d.interrupted, "subtitle was {}", d.subtitle);
        assert!(d.subtitle.contains("Interrupted"));
        std::fs::remove_dir_all(&root).ok();
    }

    /// Mid-transcription, which is the state every healthy session passes
    /// through for the minutes ASR and diarization take: audio, a `raw.jsonl`
    /// created the instant the transcriber claimed the session, no growing
    /// wav and not one transcript line. Keying `interrupted` off transcript
    /// lines rather than off `raw.jsonl` marked all of it Interrupted — in
    /// the very pane this step exists to make honest. Sessions from before
    /// capture and transcription were split have no `session.json` at this
    /// point either, which is the shape drawn here.
    #[test]
    fn a_session_still_transcribing_is_not_interrupted() {
        let root = scratch("transcribing");
        let dir = root.join("2026-08-30T1600");
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        // The finished tracks survive; the native scratch wavs are already gone.
        std::fs::write(dir.join("audio").join("room.wav"), b"RIFF").unwrap();
        std::fs::write(dir.join("audio").join("call.wav"), b"RIFF").unwrap();
        // Created the instant transcription begins, and empty until the first
        // segment.
        std::fs::write(dir.join("raw.jsonl"), b"").unwrap();

        let d = describe(&dir);
        assert!(
            !d.interrupted,
            "a session mid-transcription must not read as interrupted; subtitle was {}",
            d.subtitle
        );
        assert!(!d.subtitle.contains("Interrupted"), "{}", d.subtitle);
        std::fs::remove_dir_all(&root).ok();
    }

    const META: &str = "{\"id\":\"2026-08-30T2000\",\"name\":null,\"started_at\":\"2026-08-30T20:00:00+10:00\",\"ended_at\":\"2026-08-30T20:00:20+10:00\",\"duration_s\":20.0,\"device_hz\":48000,\"channels\":1,\"mic_channels\":1,\"apps\":[],\"model\":\"m\"}";

    /// A run that reached its end wrote `transcript.md`. It heard nothing, and
    /// that is a different fact from a capture that was killed — reporting it
    /// as a loss would send someone looking for audio that is exactly where
    /// they left it.
    #[test]
    fn a_finished_recording_that_heard_nothing_is_not_interrupted() {
        let root = scratch("silent");
        let dir = root.join("2026-08-30T2000");
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        std::fs::write(dir.join("audio").join("room.wav"), b"RIFF").unwrap();
        std::fs::write(dir.join("raw.jsonl"), b"").unwrap();
        std::fs::write(dir.join("session.json"), META).unwrap();
        std::fs::write(dir.join("transcript.md"), "# nothing\n").unwrap();

        let d = describe(&dir);
        assert!(!d.interrupted, "subtitle was {}", d.subtitle);
        assert!(d.completed);
        assert!(!d.queued);
        assert!(
            d.subtitle.contains("no speech recognised"),
            "{}",
            d.subtitle
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// The state the capture/transcription split created: audio and
    /// `session.json` written, no `raw.jsonl` because no transcriber has
    /// claimed it yet. Neither a loss nor a silent recording — it is waiting
    /// on the queue, and must say so.
    #[test]
    fn a_captured_session_awaiting_its_transcript_says_so() {
        let root = scratch("queued");
        let dir = root.join("2026-08-30T2000");
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        std::fs::write(dir.join("audio").join("room.wav"), b"RIFF").unwrap();
        std::fs::write(dir.join("session.json"), META).unwrap();
        std::fs::write(dir.join("status"), "captured\n").unwrap();

        let d = describe(&dir);
        assert!(d.queued);
        assert!(!d.interrupted, "subtitle was {}", d.subtitle);
        assert!(!d.completed);
        assert!(
            d.subtitle.contains("waiting to transcribe"),
            "{}",
            d.subtitle
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// An empty directory is not a corpse — nothing was captured — so it must
    /// not be labelled as one.
    #[test]
    fn an_empty_directory_is_not_interrupted() {
        let root = scratch("empty");
        let dir = root.join("2026-08-30T1600");
        std::fs::create_dir_all(&dir).unwrap();
        let d = describe(&dir);
        assert!(!d.interrupted);
        assert!(d.subtitle.contains("no transcript yet"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_named_session_is_titled_by_its_name() {
        let root = scratch("named");
        let dir = root.join("2026-08-30T1700");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("session.json"),
            "{\"id\":\"2026-08-30T1700\",\"name\":\"Export spec\",\"started_at\":\"2026-08-30T17:00:00+10:00\",\"ended_at\":\"2026-08-30T17:01:00+10:00\",\"duration_s\":60.0,\"device_hz\":48000,\"channels\":1,\"mic_channels\":1,\"apps\":[],\"model\":\"m\"}",
        )
        .unwrap();
        assert_eq!(describe(&dir).title, "Export spec");
        std::fs::remove_dir_all(&root).ok();
    }

    /// Old `session.json` files have no `warnings` key, and must still load.
    #[test]
    fn a_session_written_before_warnings_existed_still_parses() {
        let root = scratch("nowarnings");
        let dir = root.join("2026-08-30T1800");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("session.json"),
            "{\"id\":\"2026-08-30T1800\",\"name\":null,\"started_at\":\"2026-08-30T18:00:00+10:00\",\"ended_at\":\"2026-08-30T18:00:30+10:00\",\"duration_s\":30.0,\"device_hz\":48000,\"channels\":1,\"mic_channels\":1,\"apps\":[],\"model\":\"m\"}",
        )
        .unwrap();
        assert!(describe(&dir).warnings.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    /// The cache key has to notice an append to `edits.jsonl`, because naming
    /// a speaker is an append and does not touch the directory's own mtime.
    #[test]
    fn the_stamp_moves_when_edits_are_appended() {
        let root = scratch("stamp");
        let dir = root.join("2026-08-30T1900");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("edits.jsonl"), b"a\n").unwrap();
        let before = stamp_of(&dir);
        filetime_set(
            &dir.join("edits.jsonl"),
            std::time::SystemTime::now() + std::time::Duration::from_secs(5),
        );
        assert!(stamp_of(&dir) > before);
        std::fs::remove_dir_all(&root).ok();
    }

    fn shot(elapsed_s: u64, meter: MeterPhase, arriving: bool) -> LiveShot {
        LiveShot {
            id: "2026-08-30T1412".into(),
            dir: PathBuf::from("/tmp/2026-08-30T1412"),
            app: Some("us.zoom.xos".into()),
            elapsed_s,
            room: 0.31,
            call: 0.87,
            audio_arriving: arriving,
            meter,
            stoppable: meter == MeterPhase::Capturing,
        }
    }

    /// The pinned row says what is being recorded, where it is going, and how
    /// long it has been going for.
    #[test]
    fn the_live_row_reads_as_a_recording_in_progress() {
        let (title, subtitle) = shot(252, MeterPhase::Capturing, true).row_lines();
        assert_eq!(title, "● Recording — 04:12");
        assert_eq!(subtitle, "Capturing us.zoom.xos → 2026-08-30T1412");
    }

    /// The one failure this app cannot afford to be quiet about: a denied tap
    /// returns correctly-shaped zeros rather than an error, so "nothing is
    /// arriving" is the only thing that separates it from a quiet room.
    #[test]
    fn a_silent_capture_says_so_in_the_row() {
        let (_, subtitle) = shot(9, MeterPhase::Capturing, false).row_lines();
        assert!(subtitle.ends_with("no audio arriving"), "{subtitle}");
    }

    /// Once the worker moves on, the clock stops being the news — and a row
    /// still saying "Recording" over a capture that has finished would be the
    /// display lying about the worker, which is what the meter exists to stop.
    #[test]
    fn the_row_follows_the_worker_not_the_phase() {
        let (title, _) = shot(252, MeterPhase::Transcribing, true).row_lines();
        assert_eq!(title, "● Transcribing…");
        let (title, _) = shot(252, MeterPhase::Diarizing, true).row_lines();
        assert_eq!(title, "● Separating voices…");
    }

    /// The whole point of step 4's per-interval peaks: the live view reads
    /// those, not the cumulative ones `silent_tap_advice` guards the capture
    /// with, and not the filesystem.
    #[test]
    fn the_live_view_reads_the_shared_meter_and_nothing_else() {
        let root = crate::state::test_support::scratch("window-live");
        let (phase, _tx) = crate::state::test_support::recording(
            &root,
            Some("us.zoom.xos"),
            crate::state::Declined::default(),
        );
        let live = phase.live().expect("a Recording owns a Live");

        // Exactly what the worker thread writes once a second.
        live.meter.elapsed_ms.store(65_000, Ordering::Relaxed);
        live.meter.room_peak_milli.store(310, Ordering::Relaxed);
        live.meter.call_peak_milli.store(870, Ordering::Relaxed);
        live.meter.audio_arriving.store(true, Ordering::Relaxed);

        let shot = LiveShot::of(live, phase.kind());
        assert_eq!(shot.clock(), "01:05");
        assert!((shot.room - 0.31).abs() < 1e-9, "room: {}", shot.room);
        assert!((shot.call - 0.87).abs() < 1e-9, "call: {}", shot.call);
        assert!(
            shot.stoppable,
            "a Recording phase is the one Stop is legal in"
        );
        assert_eq!(
            shot.id,
            live.dir.path().file_name().unwrap().to_string_lossy()
        );

        // A mic that goes quiet falls back within a second, because the worker
        // resets the interval peak every time it writes one. Reading the
        // cumulative peaks here would hold 0.31 for the rest of the capture.
        live.meter.room_peak_milli.store(0, Ordering::Relaxed);
        assert_eq!(LiveShot::of(live, phase.kind()).room, 0.0);

        std::fs::remove_dir_all(&root).ok();
    }

    /// Stop is the phase's business, the label is the meter's. A phase that
    /// has left `Recording` must not offer a Stop that would do nothing.
    #[test]
    fn stop_is_offered_only_while_the_phase_is_recording() {
        let root = crate::state::test_support::scratch("window-stop");
        let (phase, _tx) =
            crate::state::test_support::recording(&root, None, crate::state::Declined::default());
        let stopped = phase.stop().unwrap_or_else(|(_, e)| panic!("{e}"));
        let live = stopped.live().unwrap();
        assert!(!LiveShot::of(live, stopped.kind()).stoppable);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn relative_dates_read_like_dates() {
        let now = Local::now();
        assert!(relative(now).starts_with("Today "));
        assert!(relative(now - chrono::Duration::days(1)).starts_with("Yesterday "));
        assert!(!relative(now - chrono::Duration::days(400)).contains("Today"));
    }

    /// `libc::utimes` rather than a dev-dependency: one call, and the tests
    /// above need nothing else from it.
    fn filetime_set(p: &Path, t: SystemTime) {
        let secs = t
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0) as libc::time_t;
        let tv = libc::timeval {
            tv_sec: secs,
            tv_usec: 0,
        };
        let times = [tv, tv];
        let c = std::ffi::CString::new(p.to_string_lossy().as_bytes()).unwrap();
        unsafe {
            libc::utimes(c.as_ptr(), times.as_ptr());
        }
    }
}
