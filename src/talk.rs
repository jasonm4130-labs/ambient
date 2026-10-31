//! `ambient talk`: one spoken turn with the user's own agent, in a
//! short-lived child.
//!
//! The app holds the microphone and buffers a push-to-talk hold in memory;
//! on release it pipes the audio here. This process trims it with the VAD,
//! transcribes it with Parakeet on this Mac, posts the text as a question
//! through a [`Target`], waits for the answer, and speaks it through the
//! assistant's voice helper. The audio never leaves the process and is never
//! written anywhere: only the text goes to the target.
//!
//! The target is the [`mailbox`]: the question waits in `talk/` beside the
//! config file until the `ambient mcp` process listening in the user's agent
//! session ([`listen`]) hands it over, and the agent answers with
//! `talk_reply`. Ambient holds no model for this; the answer is the agent's,
//! as the live assistant's judgement is (ADR 0018, ADR 0019).
//!
//! It runs as a child for the reason finalization does
//! ([`crate::finalize`]): the recogniser's memory goes back to the system
//! when the process exits, rather than staying in the app's footprint.
//!
//! Its stdout is one JSON event per line for the parent; people read stderr.
//!
//! ```text
//! {"event":"heard","text":"…","speech_s":1.8,"load_ms":420,"text_ms":610}
//! {"event":"nothing","speech_s":0.1}               didn't catch that; nothing sent
//! {"event":"queued","reason":"…"}                  no agent is listening; the question still waits
//! {"event":"sent","id":"ambient-…","request_id":"ambient-…","announced":true}
//! {"event":"reply","id":"…","text":"…","speech":"…","reply_ms":5200}
//! {"event":"spoken","first_audio_ms":60,"audio_s":3.1}
//! {"event":"text_only","reason":"…"}               the reply stays text
//! {"event":"dry_run","would_say":"…"}
//! {"event":"no_reply","id":"…","waited_s":300}
//! ```
//!
//! SIGTERM or SIGINT stops it at once and silences the voice mid-word, which
//! is how the parent barges in when the user holds the key again;
//! `ambient talk --stop` sends that signal to whichever turn is speaking.
//! SIGUSR1 stops a reply being said, as SIGTERM would, because a recording
//! started or a call is waiting to be recorded; a turn not yet speaking
//! ignores it, and checks [`is_paused_in`] just before it would speak.

use crate::assist::{voice, Speaker, MAX_SPEECH_CHARS};
use crate::config::Config;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};
use std::time::{Duration, Instant};

pub mod app;
pub mod listen;
pub mod mailbox;

/// The subcommand the child runs under.
pub const SUBCOMMAND: &str = "talk";

/// Less speech than this is a slip of the key, not a question.
pub const MIN_SPEECH_S: f64 = 0.4;

/// How often the target is asked for the reply.
const POLL: Duration = Duration::from_secs(1);

/// How long a turn waits for its reply by default. After it, the question is
/// marked expired and a late `talk_reply` is told so.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// What a queued turn tells the user to do.
pub const NO_LISTENER: &str = "no agent is listening: in Claude Code, run /mcp__ambient__talk";

/// In the sessions folder, kept fresh by the app while talking is paused for
/// a call waiting to be recorded or a recording starting.
const PAUSE_FILE: &str = ".talk-paused";

/// Mark talking as paused, or not, for the workers under `root`. The app
/// touches the mark every tick while paused, so one left by a crash goes
/// stale within seconds.
pub fn set_paused(root: &Path, on: bool) -> std::io::Result<()> {
    let mark = root.join(PAUSE_FILE);
    if on {
        std::fs::create_dir_all(root)?;
        std::fs::write(mark, "")
    } else {
        match std::fs::remove_file(mark) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// Whether a reply may not be said now: a recording is in progress under
/// `root`, or the app has marked talking as paused.
pub fn is_paused_in(root: &Path) -> bool {
    crate::session::live_session_in(root).is_some()
        || crate::session::is_growing(&root.join(PAUSE_FILE))
}

/// How long a reply waits for the live assistant to finish speaking before
/// it stays text.
const SPEAKING_WAIT: Duration = Duration::from_secs(10);

pub const USAGE: &str = "\
ambient talk (--wav <file> | --pcm [--rate <hz>]) [--dry-run] [--no-play]
             [--timeout <s>]
ambient talk --stop

  --wav <file>   a recorded question, any rate or channel count
  --pcm          raw 32-bit float little-endian mono on stdin, until EOF
  --rate <hz>    the --pcm sample rate (default 16000)
  --dry-run      post the question and wait for the answer, but print it
                 instead of speaking it
  --no-play      speak through the voice helper without playing the audio
  --timeout <s>  how long to wait for the answer (default 300)
  --stop         silence the turn that is speaking now

The question goes to the agent session listening through `ambient mcp`:
in Claude Code, run /mcp__ambient__talk.
";

/// Where a transcript goes and its reply comes from.
pub trait Target {
    /// Whether the target can take a note now: `Some(false)` means it will
    /// sit queued until the agent is free, `None` that nobody can tell.
    fn ready(&mut self) -> Result<Option<bool>>;
    /// Send `text` as a note, idempotently under `request_id`.
    fn send(&mut self, text: &str, request_id: &str) -> Result<Sent>;
    /// The reply to note `id`, if it has arrived.
    fn reply(&mut self, id: &str) -> Result<Option<String>>;
    /// The turn stopped waiting for note `id`.
    fn expire(&mut self, _id: &str) {}
}

/// A note the target accepted.
#[derive(Debug, Clone, PartialEq)]
pub struct Sent {
    pub id: String,
    /// False when the note is saved but the agent was not told; it will be
    /// seen at the agent's next check rather than now.
    pub announced: bool,
}

/// The [`mailbox`], for the agent session listening through `ambient mcp`.
pub struct MailboxTarget {
    mailbox: mailbox::Mailbox,
    config_file: PathBuf,
    /// How long the question stays open, which is how long this turn waits.
    timeout: Duration,
}

impl MailboxTarget {
    pub fn new(config_file: &Path, timeout: Duration) -> Result<Self> {
        let mailbox = mailbox::Mailbox::at(config_file).map_err(|e| {
            anyhow!(
                "could not open the talk mailbox beside {}: {e}",
                config_file.display()
            )
        })?;
        // Whatever an earlier turn left behind (a stopped worker never
        // cleans up) is cleared before this one posts.
        mailbox.sweep();
        Ok(Self {
            mailbox,
            config_file: config_file.to_path_buf(),
            timeout,
        })
    }
}

impl Target for MailboxTarget {
    fn ready(&mut self) -> Result<Option<bool>> {
        Ok(Some(mailbox::listener(&self.config_file).is_some()))
    }

    fn send(&mut self, text: &str, request_id: &str) -> Result<Sent> {
        self.mailbox
            .post(request_id, text, std::process::id(), self.timeout)
            .map_err(|e| anyhow!("could not post the question: {e}"))?;
        Ok(Sent {
            id: request_id.to_string(),
            announced: mailbox::listener(&self.config_file).is_some(),
        })
    }

    fn reply(&mut self, id: &str) -> Result<Option<String>> {
        Ok(self.mailbox.take_reply(id))
    }

    fn expire(&mut self, id: &str) {
        self.mailbox.expire(id);
    }
}

/// Markdown made sayable: code blocks, rules and table borders dropped;
/// headings, bullets, quotes and emphasis reduced to their words; a link to
/// its text and a bare URL to "a link". A line that ends without punctuation
/// gets a full stop, so a list is read as separate sentences.
pub fn speakable(md: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut fenced = false;
    for line in md.lines() {
        let t = line.trim();
        if is_fence(t) {
            fenced = !fenced;
            continue;
        }
        if fenced || t.is_empty() || is_rule(t) {
            continue;
        }
        let t = t.trim_start_matches('#').trim_start_matches('>').trim();
        let words = inline(strip_bullet(t));
        if !words.is_empty() {
            lines.push(words);
        }
    }
    let mut out = String::new();
    for l in lines {
        if !out.is_empty() {
            if !out.ends_with(['.', '!', '?', ':', ';', ',']) {
                out.push('.');
            }
            out.push(' ');
        }
        out.push_str(&l);
    }
    out
}

/// The part of a reply that is said aloud: its first paragraph, up to the
/// first blank line or code block, after any headings. The agent is asked to
/// lead with what should be heard and to put detail after a blank line,
/// which is shown and not read out. A reply with no such paragraph is read
/// whole.
pub fn lead(md: &str) -> String {
    let mut para: Vec<&str> = Vec::new();
    let mut fenced = false;
    for line in md.lines() {
        let t = line.trim();
        if is_fence(t) {
            if !para.is_empty() {
                break;
            }
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if t.is_empty() {
            if !para.is_empty() {
                break;
            }
            continue;
        }
        if para.is_empty() && t.starts_with('#') {
            continue;
        }
        para.push(line);
    }
    if para.is_empty() {
        md.to_string()
    } else {
        para.join("\n")
    }
}

fn is_fence(t: &str) -> bool {
    t.starts_with("```") || t.starts_with("~~~")
}

fn is_rule(t: &str) -> bool {
    t.chars()
        .all(|c| matches!(c, '-' | '*' | '_' | '=' | '|' | ':' | ' '))
}

/// Markdown made readable as plain text, for the window: lines and
/// paragraphs kept, bullets drawn as "• ", numbers kept, emphasis, heading
/// marks and code ticks dropped, a link reduced to its text, and code blocks
/// kept as they are. Nothing in it needs a markdown renderer to read.
pub fn readable(md: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let (mut fenced, mut gap) = (false, false);
    for line in md.lines() {
        let t = line.trim();
        if is_fence(t) {
            fenced = !fenced;
            continue;
        }
        let shown = if fenced {
            line.trim_end().to_string()
        } else if t.is_empty() || is_rule(t) {
            gap |= t.is_empty() && !out.is_empty();
            continue;
        } else {
            let t = t.trim_start_matches('#').trim_start_matches('>').trim();
            let (mark, rest) = bullet(t);
            let words = inline_with(rest, false);
            if words.is_empty() {
                continue;
            }
            format!("{mark}{words}")
        };
        if gap {
            out.push(String::new());
            gap = false;
        }
        out.push(shown);
    }
    out.join("\n")
}

/// A list item's marker as [`readable`] draws it, and the rest of the line.
fn bullet(t: &str) -> (String, &str) {
    let rest = strip_bullet(t);
    if rest.len() == t.len() {
        return (String::new(), t);
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        (format!("{}. ", &t[..digits]), rest)
    } else {
        ("• ".to_string(), rest)
    }
}

fn strip_bullet(t: &str) -> &str {
    for b in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(b) {
            return rest.trim_start_matches("[ ] ").trim_start_matches("[x] ");
        }
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        let rest = &t[digits..];
        if let Some(r) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return r;
        }
    }
    t
}

/// Links, images, emphasis, code spans, table pipes and URLs within a line.
fn inline(t: &str) -> String {
    inline_with(t, true)
}

/// [`inline`], with a bare URL said as "a link" only when `urls_as_words`.
fn inline_with(t: &str, urls_as_words: bool) -> String {
    let mut s = String::with_capacity(t.len());
    let chars: Vec<char> = t.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // `[text](url)` and `![alt](url)` keep only the text.
        if c == '[' || (c == '!' && chars.get(i + 1) == Some(&'[')) {
            let open = if c == '!' { i + 1 } else { i };
            if let Some(close) = (open + 1..chars.len()).find(|&j| chars[j] == ']') {
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(end) = (close + 2..chars.len()).find(|&j| chars[j] == ')') {
                        s.extend(&chars[open + 1..close]);
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        match c {
            '*' | '`' | '~' => {}
            '|' => s.push(' '),
            _ => s.push(c),
        }
        i += 1;
    }
    s.split_whitespace()
        .map(|w| {
            let bare = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != ':');
            if urls_as_words && (bare.starts_with("http://") || bare.starts_with("https://")) {
                "a link"
            } else {
                w.trim_matches('_')
            }
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// At most `max` characters: whole sentences if they fill at least half of
/// it, else whole words and an ellipsis.
pub fn cap(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max.saturating_sub(1)).collect();
    if let Some(i) = head.rfind(['.', '!', '?']).filter(|&i| i >= head.len() / 2) {
        return head[..=i].to_string();
    }
    let cut = head.rfind(char::is_whitespace).unwrap_or(head.len());
    format!("{}…", head[..cut].trim_end())
}

/// What the recogniser made of the audio.
#[derive(Debug, Clone, PartialEq)]
pub struct Heard {
    pub text: String,
    /// Seconds the VAD called speech, before padding.
    pub speech_s: f64,
    pub load_ms: u64,
}

/// VAD then Parakeet, from the models under `models`. Too little speech
/// comes back as empty text without loading the recogniser.
pub fn hear(samples: &[f32], models: &Path) -> Result<Heard> {
    let started = Instant::now();
    let files = crate::session::model_files(models);
    let mut vad = crate::vad::Vad::load(&files.vad.display().to_string())
        .with_context(|| format!("loading {}", files.vad.display()))?;
    vad.pad_ms = 0;
    let speech_s = vad
        .segments(samples)?
        .iter()
        .fold(0.0, |total, s| total + s.seconds());
    if speech_s < MIN_SPEECH_S {
        return Ok(Heard {
            text: String::new(),
            speech_s,
            load_ms: started.elapsed().as_millis() as u64,
        });
    }
    vad.pad_ms = crate::vad::PAD_MS;
    let chunks = vad.chunks(samples, 30)?;
    drop(vad);
    let mut rec = crate::asr::Recognizer::load(&files.asr_dir.display().to_string())
        .with_context(|| format!("loading {}", files.asr_dir.display()))?;
    let load_ms = started.elapsed().as_millis() as u64;
    let text = rec.transcribe_chunked(samples, &chunks)?;
    Ok(Heard {
        text: text.trim().to_string(),
        speech_s,
        load_ms,
    })
}

/// What happened to the reply.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Spoken,
    TextOnly(String),
    DryRun,
    NoReply,
}

/// Everything a turn needs besides the transcript, so a test can stand each
/// part in.
pub struct Turn<'a> {
    pub target: &'a mut dyn Target,
    /// `None` is a dry run: the reply is printed, not said.
    pub speaker: Option<&'a mut dyn Speaker>,
    pub lock: PathBuf,
    /// Whether talking is paused for a recording or a call waiting to be
    /// recorded, asked again just before speaking.
    pub paused: &'a dyn Fn() -> bool,
    pub poll: Duration,
    pub timeout: Duration,
    pub out: &'a mut dyn Write,
}

fn emit(out: &mut dyn Write, v: Value) {
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

/// Send `text`, wait for its reply, and say it.
pub fn converse(text: &str, turn: Turn<'_>) -> Result<Outcome> {
    let Turn {
        target,
        mut speaker,
        lock,
        paused,
        poll,
        timeout,
        out,
    } = turn;
    match target.ready() {
        Ok(Some(true)) => {}
        Ok(Some(false)) => emit(out, json!({"event": "queued", "reason": NO_LISTENER})),
        Ok(None) => emit(
            out,
            json!({"event": "queued", "reason": "whether an agent is listening is unknown"}),
        ),
        Err(e) => emit(out, json!({"event": "queued", "reason": format!("{e:#}")})),
    }
    let request_id = request_id();
    let sent = target.send(text, &request_id)?;
    emit(
        out,
        json!({"event": "sent", "id": sent.id, "request_id": request_id,
            "announced": sent.announced}),
    );
    let sent_at = Instant::now();
    // The voice loads while the agent thinks, so it is ready when the reply is.
    if let Some(s) = speaker.as_deref_mut() {
        if let Err(e) = s.warm() {
            eprintln!("  talk: the voice did not start: {e:#}");
        }
    }
    let reply = loop {
        match target.reply(&sent.id) {
            Ok(Some(r)) => break r,
            Ok(None) => {}
            Err(e) => eprintln!("  talk: reading replies: {e:#}"),
        }
        if sent_at.elapsed() >= timeout {
            target.expire(&sent.id);
            emit(
                out,
                json!({"event": "no_reply", "id": sent.id,
                    "waited_s": sent_at.elapsed().as_secs()}),
            );
            return Ok(Outcome::NoReply);
        }
        std::thread::sleep(poll);
    };
    let speech = cap(&speakable(&lead(&reply)), MAX_SPEECH_CHARS);
    emit(
        out,
        json!({"event": "reply", "id": sent.id, "text": reply, "speech": speech,
            "reply_ms": sent_at.elapsed().as_millis() as u64}),
    );
    let Some(speaker) = speaker else {
        emit(out, json!({"event": "dry_run", "would_say": speech}));
        return Ok(Outcome::DryRun);
    };
    let text_only = |out: &mut dyn Write, why: String| {
        emit(out, json!({"event": "text_only", "reason": why}));
        Ok(Outcome::TextOnly(why))
    };
    if speech.is_empty() {
        return text_only(out, "the reply has nothing to say aloud".into());
    }
    let _held = match voice::SpeakingLock::acquire(&lock, "talk", SPEAKING_WAIT) {
        Ok(l) => l,
        Err(e) => return text_only(out, format!("{e:#}")),
    };
    // From here a hush stops the process, so this is the last word on it.
    let _unlinked_on_signal = LockOnSignal::arm(&lock);
    if paused() {
        return text_only(
            out,
            "talking is paused for a recording or a call, so the reply is not said".into(),
        );
    }
    match speaker.say(&speech, "neutral") {
        Ok(s) => {
            emit(
                out,
                json!({"event": "spoken", "first_audio_ms": s.first_audio_ms,
                    "audio_s": s.audio_s}),
            );
            Ok(Outcome::Spoken)
        }
        Err(e) => text_only(out, format!("the voice failed: {e:#}")),
    }
}

/// A fresh `ambient-<hex>` id, unique per turn, so a retried send of the
/// same turn is one note.
fn request_id() -> String {
    let mut bytes = [0u8; 12];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    if read.is_err() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
            ^ u128::from(std::process::id());
        bytes.copy_from_slice(&n.to_le_bytes()[..12]);
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("ambient-{hex}")
}

/// The running voice helper's process group, for the signal handler.
static HELPER: AtomicI32 = AtomicI32::new(0);

/// The speaking lock this process holds, as a C path, or null; the signal
/// handler removes it so a stop never leaves it naming a pid that could be
/// reused.
static HELD_LOCK: AtomicPtr<libc::c_char> = AtomicPtr::new(std::ptr::null_mut());

/// Publishes the held lock's path to the signal handler until dropped,
/// which must happen before the lock itself is released.
struct LockOnSignal;

impl LockOnSignal {
    fn arm(lock: &Path) -> Option<LockOnSignal> {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(lock.as_os_str().as_bytes()).ok()?;
        // Leaked: the handler may read it at any moment, and a turn holds
        // the lock once.
        HELD_LOCK.store(path.into_raw(), Ordering::SeqCst);
        Some(LockOnSignal)
    }
}

impl Drop for LockOnSignal {
    fn drop(&mut self) {
        HELD_LOCK.store(std::ptr::null_mut(), Ordering::SeqCst);
    }
}

extern "C" fn interrupted(_: libc::c_int) {
    let pg = HELPER.load(Ordering::SeqCst);
    let lock = HELD_LOCK.load(Ordering::SeqCst);
    // SAFETY: killpg, unlink and _exit are async-signal-safe, and a non-null
    // lock path is a leaked, NUL-terminated CString.
    unsafe {
        if pg > 0 {
            libc::killpg(pg, libc::SIGKILL);
        }
        if !lock.is_null() {
            libc::unlink(lock);
        }
        libc::_exit(130);
    }
}

extern "C" fn hushed(sig: libc::c_int) {
    if !HELD_LOCK.load(Ordering::SeqCst).is_null() {
        interrupted(sig);
    }
}

fn on_interrupt() {
    let handler = interrupted as extern "C" fn(libc::c_int) as libc::sighandler_t;
    let hush = hushed as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: installs handlers that only read atomics and call
    // async-signal-safe functions.
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGUSR1, hush);
    }
}

/// `ambient talk --stop`: signal the turn that holds the speaking lock.
fn stop(config_file: &Path) -> Result<()> {
    match voice::SpeakingLock::holder(&voice::SpeakingLock::path(config_file)) {
        None => eprintln!("nothing is speaking"),
        Some(h) if h.who == "talk" => {
            // SAFETY: plain syscall to a pid read from the lock, which names
            // a live `ambient talk` process.
            unsafe { libc::kill(h.pid as libc::pid_t, libc::SIGTERM) };
            eprintln!("stopped the reply (pid {})", h.pid);
        }
        Some(h) => bail!(
            "the {} is speaking (pid {}); `ambient talk --stop` only stops talk replies",
            h.who,
            h.pid
        ),
    }
    Ok(())
}

enum Input {
    Wav(PathBuf),
    Pcm,
}

/// Read 32-bit float little-endian samples until end of file.
fn read_pcm(mut from: impl Read) -> Result<Vec<f32>> {
    let mut bytes = Vec::new();
    from.read_to_end(&mut bytes)?;
    if bytes.len() % 4 != 0 {
        bail!(
            "--pcm got {} bytes, not a whole number of 32-bit samples",
            bytes.len()
        );
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// The command line: `ambient talk …`.
pub fn main(args: Vec<String>) -> Result<()> {
    let mut input = None;
    let mut rate = 16_000u32;
    let (mut dry_run, mut no_play) = (false, false);
    let mut timeout = DEFAULT_TIMEOUT;
    let mut args = args.into_iter();
    let value = |args: &mut std::vec::IntoIter<String>, flag: &str| {
        args.next()
            .ok_or_else(|| anyhow!("{flag} needs a value\n\n{USAGE}"))
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--stop" => return stop(&crate::config::path()),
            "--wav" => input = Some(Input::Wav(value(&mut args, "--wav")?.into())),
            "--pcm" => input = Some(Input::Pcm),
            "--rate" => rate = value(&mut args, "--rate")?.parse()?,
            "--dry-run" => dry_run = true,
            "--no-play" => no_play = true,
            "--timeout" => timeout = Duration::from_secs(value(&mut args, "--timeout")?.parse()?),
            other => bail!("unexpected argument {other:?}\n\n{USAGE}"),
        }
    }
    let input = input.ok_or_else(|| anyhow!("give --wav <file> or --pcm\n\n{USAGE}"))?;
    let cfg = Config::load();
    let mut target = MailboxTarget::new(&crate::config::path(), timeout)?;
    on_interrupt();
    let paused = || is_paused_in(&crate::session::home());
    // Talking is paused while a recording runs: the mic is shared, so the
    // question would land in the meeting, and the answer would be heard.
    if crate::session::live_session().is_some() {
        bail!("a recording is in progress; talking is paused until it stops");
    }

    let (raw, from_hz) = match input {
        Input::Wav(p) => crate::resample::read_wav_any(&p)?,
        Input::Pcm => (read_pcm(std::io::stdin().lock())?, rate),
    };
    // The user has let go: everything from here is what they wait through.
    let released = Instant::now();
    let samples = crate::resample::to_16k(&raw, from_hz)?;
    drop(raw);
    let heard = hear(&samples, &crate::session::models_root()?)?;
    drop(samples);
    let mut out = std::io::stdout().lock();
    if heard.text.is_empty() {
        eprintln!("  talk: didn't catch that");
        emit(
            &mut out,
            json!({"event": "nothing", "speech_s": heard.speech_s}),
        );
        return Ok(());
    }
    emit(
        &mut out,
        json!({"event": "heard", "text": heard.text, "speech_s": heard.speech_s,
            "load_ms": heard.load_ms, "text_ms": released.elapsed().as_millis() as u64}),
    );

    let mut voice = (!dry_run).then(|| {
        let engine = voice::resolve_engine(cfg.voice.engine, voice::total_ram());
        let dir = cfg
            .voice
            .helper_dir
            .clone()
            .unwrap_or_else(voice::default_helper_dir);
        let extra = if no_play {
            vec!["--no-play".to_string()]
        } else {
            Vec::new()
        };
        let (program, args) = voice::helper_command(&dir, engine, &cfg.voice, &extra);
        voice::Voice::new(program, args, voice::Limits::default()).with_pid_slot(&HELPER)
    });
    let outcome = converse(
        &heard.text,
        Turn {
            target: &mut target,
            speaker: voice.as_mut().map(|v| v as &mut dyn Speaker),
            lock: voice::SpeakingLock::path(&crate::config::path()),
            paused: &paused,
            poll: POLL,
            timeout,
            out: &mut out,
        },
    )?;
    if let Outcome::TextOnly(why) = &outcome {
        eprintln!("  talk: {why}");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
