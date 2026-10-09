//! `ambient talk`: one spoken turn with an agent, in a short-lived child.
//!
//! The app holds the microphone and buffers a push-to-talk hold in memory;
//! on release it pipes the audio here. This process trims it with the VAD,
//! transcribes it with Parakeet on this Mac, sends the text as a note through
//! a [`Target`] (firstmate's `fm-inbox.sh` today), waits for the reply to
//! that note, and speaks it through the assistant's voice helper. The audio
//! never leaves the process and is never written anywhere: only the text
//! goes to the target.
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
//! {"event":"queued","reason":"…"}                  the target is busy; the note still goes
//! {"event":"sent","id":"…","request_id":"ambient-…","announced":true}
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
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};
use std::time::{Duration, Instant};

pub mod app;

/// The subcommand the child runs under.
pub const SUBCOMMAND: &str = "talk";

/// Less speech than this is a slip of the key, not a question.
pub const MIN_SPEECH_S: f64 = 0.4;

/// How often the target is asked for the reply.
const POLL: Duration = Duration::from_secs(1);

/// How long a turn waits for its reply by default. A later reply is still
/// recorded by the target; it is only not spoken by this turn.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// In the sessions folder, kept fresh by the app while talking is paused for
/// a call waiting to be recorded or a recording starting.
const PAUSE_FILE: &str = ".talk-paused";

/// Mark talking as paused, or not, for the workers under `root`. The app
/// touches the mark every tick while paused, so one left by a crash goes
/// stale within seconds.
pub fn set_paused(root: &Path, on: bool) {
    let mark = root.join(PAUSE_FILE);
    if on {
        let _ = std::fs::write(mark, "");
    } else {
        let _ = std::fs::remove_file(mark);
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
             [--home <firstmate-home>] [--timeout <s>]
ambient talk --stop

  --wav <file>   a recorded question, any rate or channel count
  --pcm          raw 32-bit float little-endian mono on stdin, until EOF
  --rate <hz>    the --pcm sample rate (default 16000)
  --dry-run      send the note and wait for the reply, but print it instead
                 of speaking it
  --no-play      speak through the voice helper without playing the audio
  --home <dir>   the firstmate home (default: the talk.firstmate_home setting)
  --timeout <s>  how long to wait for the reply (default 300)
  --stop         silence the turn that is speaking now
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
}

/// A note the target accepted.
#[derive(Debug, Clone, PartialEq)]
pub struct Sent {
    pub id: String,
    /// False when the note is saved but the agent was not told; it will be
    /// seen at the agent's next check rather than now.
    pub announced: bool,
}

/// Firstmate, through its `bin/fm-inbox.sh`: `note --request-id` to send,
/// `receipts` to find the reply, `ready` to ask whether the primary is
/// there. All three are local file operations; none makes a network call.
pub struct Firstmate {
    home: PathBuf,
    script: PathBuf,
    cursor: String,
}

impl Firstmate {
    pub fn new(home: &Path) -> Result<Self> {
        let script = home.join("bin").join("fm-inbox.sh");
        if !script.is_file() {
            bail!(
                "{} is not a firstmate home: it has no bin/fm-inbox.sh",
                home.display()
            );
        }
        Ok(Self {
            home: home.to_path_buf(),
            script,
            cursor: String::new(),
        })
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<(Option<i32>, String)> {
        let mut child = Command::new(&self.script)
            .args(args)
            .env("FM_HOME", &self.home)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("could not run {}", self.script.display()))?;
        if let Some(text) = stdin {
            let mut pipe = child.stdin.take().expect("piped stdin");
            pipe.write_all(text.as_bytes())?;
        }
        let out = child.wait_with_output()?;
        let code = out.status.code();
        if code != Some(0) && code != Some(3) {
            bail!(
                "fm-inbox.sh {} failed ({}): {}",
                args.first().copied().unwrap_or_default(),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok((code, String::from_utf8_lossy(&out.stdout).into_owned()))
    }

    fn json(&self, args: &[&str], stdin: Option<&str>) -> Result<(Option<i32>, Value)> {
        let (code, out) = self.run(args, stdin)?;
        let v = serde_json::from_str(out.trim())
            .with_context(|| format!("fm-inbox.sh {} printed {out:?}", args[0]))?;
        Ok((code, v))
    }

    /// Every reply after the cursor: receipts pages its replies oldest
    /// first, so a bounded page would trail the newest ones.
    fn receipts(&self) -> Result<Value> {
        let mut args = vec!["receipts"];
        if !self.cursor.is_empty() {
            args.extend(["--after", &self.cursor]);
        }
        args.push("--all-replies");
        Ok(self.json(&args, None)?.1)
    }
}

impl Target for Firstmate {
    fn ready(&mut self) -> Result<Option<bool>> {
        let (_, v) = self.json(&["ready"], None)?;
        Ok(v["can_receive"].as_bool())
    }

    fn send(&mut self, text: &str, request_id: &str) -> Result<Sent> {
        // Replies are read from here on, so one recorded before this note
        // cannot be mistaken for its answer.
        self.cursor = self.receipts()?["reply_cursor"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let (code, v) = self.json(
            &["note", "--request-id", request_id, "--json", "-"],
            Some(text),
        )?;
        let id = v["id"]
            .as_str()
            .ok_or_else(|| anyhow!("fm-inbox.sh note gave no id: {v}"))?
            .to_string();
        let mut announced = v["announced"].as_bool() == Some(true);
        // Exit 3 is "saved but firstmate was not woken": repair the wake for
        // this note rather than sending it again.
        if code == Some(3) || !announced {
            announced = match self.json(&["announce", "--json", &id], None) {
                Ok((_, a)) => a["announced"].as_bool() == Some(true),
                Err(e) => {
                    eprintln!("  talk: note {id} is saved but not announced: {e:#}");
                    false
                }
            };
        }
        Ok(Sent { id, announced })
    }

    fn reply(&mut self, id: &str) -> Result<Option<String>> {
        let v = self.receipts()?;
        if let Some(c) = v["reply_cursor"].as_str() {
            self.cursor = c.to_string();
        }
        Ok(v["replies"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["id"] == id)
            .and_then(|r| r["body"].as_str())
            .map(str::to_string))
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
        if t.starts_with("```") || t.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced
            || t.is_empty()
            || t.chars()
                .all(|c| matches!(c, '-' | '*' | '_' | '=' | '|' | ':' | ' '))
        {
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
            if bare.starts_with("http://") || bare.starts_with("https://") {
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
        Ok(Some(false)) => emit(
            out,
            json!({"event": "queued", "reason": "firstmate is not taking notes right now; \
                it will see this when it is"}),
        ),
        Ok(None) => emit(
            out,
            json!({"event": "queued", "reason": "firstmate's readiness is unknown"}),
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
            emit(
                out,
                json!({"event": "no_reply", "id": sent.id,
                    "waited_s": sent_at.elapsed().as_secs()}),
            );
            return Ok(Outcome::NoReply);
        }
        std::thread::sleep(poll);
    };
    let speech = cap(&speakable(&reply), MAX_SPEECH_CHARS);
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
    let mut home: Option<PathBuf> = None;
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
            "--home" => home = Some(value(&mut args, "--home")?.into()),
            "--timeout" => timeout = Duration::from_secs(value(&mut args, "--timeout")?.parse()?),
            other => bail!("unexpected argument {other:?}\n\n{USAGE}"),
        }
    }
    let input = input.ok_or_else(|| anyhow!("give --wav <file> or --pcm\n\n{USAGE}"))?;
    let cfg = Config::load();
    let home = home
        .or_else(|| cfg.talk.firstmate_home.clone())
        .ok_or_else(|| {
            anyhow!(
                "talking has nowhere to go: set the firstmate home with \
                 `ambient config talk.firstmate_home <dir>`"
            )
        })?;
    let mut target = Firstmate::new(&home)?;
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
