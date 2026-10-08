//! The live assistant: an agent in the user's own Claude Code session watches
//! the meeting being recorded through `ambient mcp`, and speaks through a
//! local voice when it has something worth adding.
//!
//! Ambient holds no model and makes no network call for this. The judgement
//! — whether to speak, and what to say — is the agent's, made on the user's
//! own subscription. What Ambient owns is everything that must hold whatever
//! the agent does:
//!
//! - **Consent.** Nothing works while `assistant` is off, and watching a
//!   meeting starts by saying out loud that an AI is listening and may speak.
//!   While it watches, a heartbeat file tells the menu bar to show it.
//! - **Restraint.** A cooldown after each utterance and a cap per meeting
//!   ([`gate::Gate`]), enforced by `speak` itself.
//! - **Hearing itself.** The assistant's voice comes back in through the
//!   microphone; those lines are dropped before the agent sees them.
//! - **The voice.** A supervised helper process ([`voice::Voice`]), kept warm
//!   while watching and stopped when watching ends.
//!
//! [`Watch`] is the state behind the four MCP tools — `watch_meeting`,
//! `wait_for_transcript`, `speak` and `stop_watching` — and [`watch_prompt`]
//! is the ready-made instruction a client offers as a slash command.

pub mod gate;
pub mod voice;

use crate::api::{self, Paths};
use crate::config::Config;
use anyhow::Result;
use gate::{is_echo, Gate, Rules};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How many earlier lines `watch_meeting` hands back as context.
const CONTEXT_LINES: usize = 40;

/// How long the assistant remembers what it said, for echo detection.
const ECHO_WINDOW: Duration = Duration::from_secs(120);

/// The longest a single `wait_for_transcript` may block, and its default.
/// A client's tool-call timeout must be longer; Claude Code's is.
const MAX_WAIT: Duration = Duration::from_secs(120);
const DEFAULT_WAIT: Duration = Duration::from_secs(30);

/// The longest an utterance may be. The assistant speaks in a meeting; a
/// paragraph is a monologue.
const MAX_SPEECH_CHARS: usize = 600;

/// How long after the agent's last call the menu bar still says the
/// assistant is listening. A wait plus a slow turn fits well inside it; an
/// agent that stopped calling without saying so lapses after it.
const ATTENTION: Duration = Duration::from_secs(MAX_WAIT.as_secs() + 60);

/// The moods `speak` accepts. A preset Qwen3-TTS speaker turns each into a
/// tone of voice and Supertonic into a non-verbal tag; a cloned voice and
/// Kokoro ignore them.
pub const EMOTIONS: [&str; 6] = [
    "neutral",
    "warm",
    "amused",
    "excited",
    "apologetic",
    "concerned",
];

/// Said once on starting to watch each meeting, before anything else.
pub fn consent_notice(name: &str) -> String {
    format!(
        "Hi everyone. Just so you know, an AI assistant called {name} is listening to this \
         meeting and may speak up."
    )
}

/// Where the voice goes. [`voice::Voice`] for real; tests record instead.
pub trait Speaker {
    fn say(&mut self, text: &str, emotion: &str) -> Result<voice::Spoken>;
    fn stop(&mut self) {}
}

impl Speaker for voice::Voice {
    fn say(&mut self, text: &str, emotion: &str) -> Result<voice::Spoken> {
        voice::Voice::say(self, text, emotion)
    }
    fn stop(&mut self) {
        voice::Voice::stop(self)
    }
}

/// Builds the speaker when watching starts, from the settings as they are
/// then, so a voice changed between meetings is the one that speaks.
pub type MakeSpeaker = Box<dyn Fn(&Config) -> Box<dyn Speaker>>;

/// The real voice helper for these settings, with `extra` passed through
/// (`ambient mcp --no-play` and `--save-audio`).
pub fn real_voice(extra: Vec<String>) -> MakeSpeaker {
    Box::new(move |cfg: &Config| {
        let engine = voice::resolve_engine(cfg.voice.engine, voice::total_ram());
        let dir = cfg
            .voice
            .helper_dir
            .clone()
            .unwrap_or_else(voice::default_helper_dir);
        let (program, args) = voice::helper_command(&dir, engine, &cfg.voice, &extra);
        Box::new(voice::Voice::new(program, args, voice::Limits::default())) as Box<dyn Speaker>
    })
}

/// How `wait_for_transcript` batches lines. People speak in bursts, and a
/// turn of the agent per line would be a turn every few seconds; a batch
/// closes when the room goes quiet for `settle`, when it has been open for
/// `longest`, or at once when someone says the assistant's name.
#[derive(Debug, Clone)]
pub struct Pacing {
    pub poll: Duration,
    pub settle: Duration,
    pub longest: Duration,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(500),
            settle: Duration::from_secs(3),
            longest: Duration::from_secs(20),
        }
    }
}

/// What the heartbeat thread writes, shared with it.
#[derive(Debug, Default)]
struct Beat {
    session: Option<String>,
    state: &'static str,
    last_call: Option<Instant>,
}

/// The meeting being watched.
struct Meeting {
    id: String,
    dir: PathBuf,
    cursor: u64,
    gate: Gate,
    /// What it said and when, for echo detection.
    said: Vec<(Instant, String)>,
}

/// The state behind the watching tools, one per `ambient mcp` process.
pub struct Watch {
    paths: Paths,
    make_speaker: MakeSpeaker,
    speaker: Option<Box<dyn Speaker>>,
    meeting: Option<Meeting>,
    pacing: Pacing,
    beat: Arc<Mutex<Beat>>,
    beating: Arc<AtomicBool>,
}

/// A tool that ran and could not do what was asked. The text is for the
/// agent: it says what happened and what to do next.
pub type Refused = String;

impl Watch {
    pub fn new(paths: Paths, make_speaker: MakeSpeaker) -> Self {
        Self {
            paths,
            make_speaker,
            speaker: None,
            meeting: None,
            pacing: Pacing::default(),
            beat: Arc::default(),
            beating: Arc::default(),
        }
    }

    pub fn with_pacing(mut self, pacing: Pacing) -> Self {
        self.pacing = pacing;
        self
    }

    /// The heartbeat file, beside the config file.
    pub fn heartbeat_path(config_file: &Path) -> PathBuf {
        config_file.with_file_name("assistant.json")
    }

    /// The four tools, as `tools/list` reports them.
    pub fn tools() -> Vec<Value> {
        vec![
            json!({
                "name": "watch_meeting",
                "description": "Start watching the meeting Ambient is recording, as the live \
                    assistant. Announces out loud that an AI is listening and may speak, then \
                    returns recent lines for context. Fails if the live assistant is turned off \
                    or nothing is being recorded. Call once, then loop on wait_for_transcript.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session": {"type": "string", "description": "Watch this session rather than whichever is being recorded."},
                    },
                },
            }),
            json!({
                "name": "wait_for_transcript",
                "description": "Block until new lines are said in the watched meeting, then \
                    return them. Returns a burst of speech once the room pauses, at once when \
                    someone says the assistant's name, or nothing after the timeout. Its own \
                    voice, heard back, is left out. `status` is `live` while the meeting goes \
                    on; on `ended` or `off`, stop.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "timeout_s": {"type": "integer", "minimum": 1, "maximum": MAX_WAIT.as_secs(), "description": "Longest to wait for a new line (default 30)."},
                    },
                },
            }),
            json!({
                "name": "speak",
                "description": "Say something out loud in the watched meeting, through the \
                    assistant's local voice. One to three short spoken sentences: plain words, \
                    no lists, markdown, links or code. Returns once it has been said. Refused \
                    during the cooldown after each utterance and past the per-meeting cap.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": {"type": "string", "description": "What to say."},
                        "emotion": {"type": "string", "enum": EMOTIONS, "description": "The tone to say it in (default neutral)."},
                    },
                    "required": ["text"],
                },
            }),
            json!({
                "name": "stop_watching",
                "description": "Stop watching the meeting: the listening notice goes and the \
                    voice is unloaded.",
                "inputSchema": {"type": "object", "properties": {}},
            }),
        ]
    }

    /// Run one of [`Watch::tools`]; `None` if `name` is not one of them.
    pub fn call(&mut self, name: &str, args: &Value) -> Option<Result<Value, Refused>> {
        Some(match name {
            "watch_meeting" => self.watch(args.get("session").and_then(Value::as_str)),
            "wait_for_transcript" => {
                let timeout = args
                    .get("timeout_s")
                    .and_then(Value::as_u64)
                    .map_or(DEFAULT_WAIT, Duration::from_secs)
                    .clamp(Duration::from_secs(1), MAX_WAIT);
                self.wait(timeout)
            }
            "speak" => self.speak(
                args.get("text").and_then(Value::as_str).unwrap_or(""),
                args.get("emotion").and_then(Value::as_str),
            ),
            "stop_watching" => Ok(self.stop_watching()),
            _ => return None,
        })
    }

    fn config(&self) -> Config {
        Config::load_from(&self.paths.config_file)
    }

    fn live_session(&self) -> Option<String> {
        let status = api::call("status", &json!({}), &self.paths).ok()?;
        status["live"]["id"].as_str().map(str::to_string)
    }

    fn transcript(&self, id: &str, since: u64) -> Result<(u64, Vec<Value>), Refused> {
        let v = api::call(
            "transcript",
            &json!({"session": id, "since": since}),
            &self.paths,
        )
        .map_err(|e| format!("could not read the transcript of {id}: {e:?}"))?;
        let next = v["next"].as_u64().unwrap_or(since);
        Ok((next, v["lines"].as_array().cloned().unwrap_or_default()))
    }

    fn off_message() -> Refused {
        "The live assistant is turned off in Ambient. Ask the user to turn on Live Assistant \
         in Ambient's menu bar (or run `ambient config assistant on`), and do not retry until \
         they say they have."
            .into()
    }

    /// `watch_meeting`.
    pub fn watch(&mut self, session: Option<&str>) -> Result<Value, Refused> {
        let cfg = self.config();
        if !cfg.assistant.enabled {
            self.leave("the assistant is off");
            return Err(Self::off_message());
        }
        let live = self.live_session();
        let id = match (session, &live) {
            (Some(s), _) => s.to_string(),
            (None, Some(l)) => l.clone(),
            (None, None) => {
                return Err(
                    "Ambient is not recording a meeting right now. Ask the user to \
                            start recording, then call watch_meeting again."
                        .into(),
                )
            }
        };
        if live.as_deref() != Some(id.as_str()) {
            return Err(format!(
                "Session {id} is not being recorded right now, so there is nothing to watch."
            ));
        }
        let (next, lines) = self.transcript(&id, 0)?;
        if self.meeting.as_ref().map(|m| m.id.as_str()) != Some(id.as_str()) {
            self.leave("a new meeting was watched");
            let dir = self.paths.root().join(&id);
            let notice = consent_notice(&cfg.assistant.name);
            self.meeting = Some(Meeting {
                id: id.clone(),
                dir: dir.clone(),
                cursor: next,
                gate: {
                    let (spoken, last) = spoken_before(&dir);
                    Gate::resume(
                        Rules {
                            cooldown: Duration::from_secs(cfg.assistant.cooldown_s.into()),
                            max_per_meeting: cfg.assistant.max_per_meeting,
                        },
                        spoken,
                        last,
                    )
                },
                // The notice is not remembered for echo detection: it
                // invites questions by name that share most of its words.
                said: Vec::new(),
            });
            self.attend("listening");
            log(&dir, json!({"event": "joined", "notice": notice}));
            let speaker = self
                .speaker
                .get_or_insert_with(|| (self.make_speaker)(&cfg));
            if let Err(e) = speaker.say(&notice, "warm") {
                // The menu bar's notice still stands, but a spoken notice is
                // the one everyone in the room hears: without it, no watching.
                log(
                    &dir,
                    json!({"event": "error", "stage": "notice", "message": format!("{e:#}")}),
                );
                self.leave("the notice could not be spoken");
                return Err(format!(
                    "Could not say the listening notice out loud ({e:#}), so the meeting is \
                     not being watched. Tell the user."
                ));
            }
        }
        self.attend("listening");
        let recent: Vec<String> = lines
            .iter()
            .rev()
            .take(CONTEXT_LINES)
            .rev()
            .map(render)
            .collect();
        let m = self.meeting.as_ref().expect("joined above");
        Ok(json!({
            "watching": id,
            "you_are": cfg.assistant.name,
            "notice_spoken": consent_notice(&cfg.assistant.name),
            "recent": recent,
            "limits": {
                "cooldown_s": cfg.assistant.cooldown_s,
                "max_per_meeting": cfg.assistant.max_per_meeting,
                "spoken": m.gate.spoken(),
            },
            "next": "Call wait_for_transcript, and keep calling it until it reports the meeting ended.",
        }))
    }

    /// `wait_for_transcript`.
    pub fn wait(&mut self, timeout: Duration) -> Result<Value, Refused> {
        let Some(id) = self.meeting.as_ref().map(|m| m.id.clone()) else {
            return Err("Not watching a meeting. Call watch_meeting first.".into());
        };
        self.attend("listening");
        let started = Instant::now();
        let mut batch: Vec<String> = Vec::new();
        let mut heard_back = 0u32;
        let mut first_new: Option<Instant> = None;
        let mut last_new = started;
        let mut named = false;
        loop {
            let cfg = self.config();
            if !cfg.assistant.enabled {
                self.leave("the assistant was turned off");
                return Ok(json!({"status": "off", "lines": batch,
                    "next": "The user turned the live assistant off, so watching has ended. \
                        Stop; there is no need to call stop_watching."}));
            }
            if self.live_session().as_deref() != Some(id.as_str()) {
                self.leave("the meeting ended");
                return Ok(json!({"status": "ended", "lines": batch,
                    "next": "The recording stopped, so watching has ended. Stop; there is no \
                        need to call stop_watching."}));
            }
            let now = Instant::now();
            let m = self.meeting.as_ref().expect("checked above");
            let (next, lines) = self.transcript(&id, m.cursor)?;
            let m = self.meeting.as_mut().expect("checked above");
            m.cursor = next;
            m.said
                .retain(|(t, _)| now.saturating_duration_since(*t) < ECHO_WINDOW);
            let recent: Vec<&str> = m.said.iter().map(|(_, s)| s.as_str()).collect();
            for line in &lines {
                let text = line["text"].as_str().unwrap_or("").trim();
                if text.is_empty() {
                    continue;
                }
                if is_echo(text, &recent) {
                    heard_back += 1;
                    continue;
                }
                named |= mentions(text, &cfg.assistant.name);
                batch.push(render(line));
                first_new.get_or_insert(now);
                last_new = now;
            }
            let p = &self.pacing;
            let ready = match first_new {
                None => false,
                Some(first) => {
                    named
                        || now.duration_since(last_new) >= p.settle
                        || now.duration_since(first) >= p.longest
                }
            };
            if ready || now.duration_since(started) >= timeout {
                break;
            }
            std::thread::sleep(p.poll);
        }
        self.attend("listening");
        let m = self.meeting.as_ref().expect("still watching");
        let may_speak = match m.gate.may_speak(Instant::now()) {
            Ok(()) => "yes".to_string(),
            Err(h) => format!("not now: {h}"),
        };
        let mut reply = json!({
            "status": "live",
            "lines": batch,
            "may_speak": may_speak,
            "spoken": m.gate.spoken(),
        });
        if heard_back > 0 {
            reply["left_out_as_your_own_voice"] = json!(heard_back);
        }
        if named {
            reply["named_you"] = json!(true);
        }
        Ok(reply)
    }

    /// `speak`.
    pub fn speak(&mut self, text: &str, emotion: Option<&str>) -> Result<Value, Refused> {
        let text = text.trim();
        if text.is_empty() {
            return Err("Nothing to say: `text` is empty.".into());
        }
        if text.chars().count() > MAX_SPEECH_CHARS {
            return Err(format!(
                "Too long to say in a meeting ({} characters; the most is {MAX_SPEECH_CHARS}). \
                 Say it in one to three short sentences.",
                text.chars().count()
            ));
        }
        let emotion = emotion.unwrap_or("neutral");
        if !EMOTIONS.contains(&emotion) {
            return Err(format!(
                "Unknown emotion {emotion:?}; use one of {}.",
                EMOTIONS.join(", ")
            ));
        }
        if !self.config().assistant.enabled {
            self.leave("the assistant was turned off");
            return Err(Self::off_message());
        }
        let Some(m) = self.meeting.as_ref() else {
            return Err("Not watching a meeting. Call watch_meeting first.".into());
        };
        if let Err(hold) = m.gate.may_speak(Instant::now()) {
            return Err(format!("Not said: {hold}. Keep listening."));
        }
        self.attend("speaking");
        let started = Instant::now();
        let spoken = self
            .speaker
            .as_mut()
            .expect("a speaker exists while watching")
            .say(text, emotion);
        let now = Instant::now();
        let m = self.meeting.as_mut().expect("checked above");
        // Counted whether or not the voice worked: retrying a stale remark
        // later would be worse than missing it.
        m.gate.spoke(now);
        m.said.push((now, text.to_string()));
        let dir = m.dir.clone();
        let left = m
            .gate
            .rules()
            .max_per_meeting
            .saturating_sub(m.gate.spoken());
        self.attend("listening");
        match spoken {
            Ok(s) => {
                log(
                    &dir,
                    json!({"event": "spoke", "text": text, "emotion": emotion,
                        "first_audio_ms": s.first_audio_ms, "audio_s": s.audio_s,
                        "took_ms": started.elapsed().as_millis() as u64}),
                );
                Ok(json!({"said": text, "audio_s": s.audio_s, "may_still_speak": left}))
            }
            Err(e) => {
                log(
                    &dir,
                    json!({"event": "error", "stage": "voice", "text": text, "message": format!("{e:#}")}),
                );
                Err(format!("The voice failed, so this was not heard: {e:#}"))
            }
        }
    }

    /// `stop_watching`.
    pub fn stop_watching(&mut self) -> Value {
        let was = self
            .meeting
            .as_ref()
            .map(|m| (m.id.clone(), m.gate.spoken()));
        self.leave("the agent stopped watching");
        match was {
            Some((id, spoken)) => json!({"stopped_watching": id, "spoken": spoken}),
            None => json!({"stopped_watching": null, "note": "Nothing was being watched."}),
        }
    }

    fn leave(&mut self, why: &str) {
        if let Some(m) = self.meeting.take() {
            log(
                &m.dir,
                json!({"event": "left", "why": why, "spoke": m.gate.spoken()}),
            );
        }
        if let Some(s) = self.speaker.as_mut() {
            s.stop();
        }
        // Dropped so the next meeting builds its voice from the settings of
        // the day.
        self.speaker = None;
        let mut beat = self.beat.lock().unwrap_or_else(|e| e.into_inner());
        beat.session = None;
        drop(beat);
        let _ = std::fs::remove_file(Self::heartbeat_path(&self.paths.config_file));
    }

    /// The agent called: record it, write the heartbeat now, and make sure
    /// the thread that keeps it fresh is running.
    fn attend(&mut self, state: &'static str) {
        let Some(m) = &self.meeting else { return };
        {
            let mut beat = self.beat.lock().unwrap_or_else(|e| e.into_inner());
            beat.session = Some(m.id.clone());
            beat.state = state;
            beat.last_call = Some(Instant::now());
        }
        let path = Self::heartbeat_path(&self.paths.config_file);
        write_beat(&path, &self.beat);
        if !self.beating.swap(true, Ordering::SeqCst) {
            let beat = Arc::clone(&self.beat);
            let beating = Arc::clone(&self.beating);
            std::thread::spawn(move || {
                while beating.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_secs(2));
                    if beating.load(Ordering::SeqCst) {
                        write_beat(&path, &beat);
                    }
                }
            });
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.leave("the MCP server stopped");
        self.beating.store(false, Ordering::SeqCst);
    }
}

/// Write the heartbeat the menu bar reads, or remove it once the agent has
/// gone quiet for longer than [`ATTENTION`].
fn write_beat(path: &Path, beat: &Mutex<Beat>) {
    let beat = beat.lock().unwrap_or_else(|e| e.into_inner());
    let attending = beat.last_call.is_some_and(|t| t.elapsed() < ATTENTION);
    let Some(session) = beat.session.as_ref().filter(|_| attending) else {
        let _ = std::fs::remove_file(path);
        return;
    };
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let v = json!({
        "pid": std::process::id(),
        "session": session,
        "state": beat.state,
        "updated_at": at,
    });
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// One transcript line as the agent reads it: `[mm:ss] who: text`.
fn render(line: &Value) -> String {
    let who = match line["speaker"].as_str() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => match line["track"].as_str() {
            Some("call") => "someone on the call".into(),
            _ => "someone in the room".into(),
        },
    };
    let s = line["start_ms"].as_u64().unwrap_or(0) / 1000;
    let text = line["text"].as_str().unwrap_or("").trim();
    format!("[{:02}:{:02}] {who}: {text}", s / 60, s % 60)
}

/// Whether `text` says `name` as whole words, ignoring case.
fn mentions(text: &str, name: &str) -> bool {
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let name = words(name);
    !name.is_empty() && words(text).windows(name.len()).any(|w| w == name)
}

/// How often the assistant has already spoken in the meeting at `dir`, and
/// when it last did, from its `assistant.jsonl`. A failed voice counts, as it
/// does in `speak`, so the limits hold across watches and processes.
fn spoken_before(dir: &Path) -> (u32, Option<Instant>) {
    let text = std::fs::read_to_string(dir.join("assistant.jsonl")).unwrap_or_default();
    let mut spoken = 0;
    let mut last: Option<chrono::DateTime<chrono::FixedOffset>> = None;
    for event in text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        let counts =
            event["event"] == "spoke" || (event["event"] == "error" && event["stage"] == "voice");
        if !counts {
            continue;
        }
        spoken += 1;
        if let Some(at) = event["at"]
            .as_str()
            .and_then(|a| chrono::DateTime::parse_from_rfc3339(a).ok())
        {
            last = last.max(Some(at));
        }
    }
    let last = last.and_then(|at| {
        let ago = (chrono::Local::now().fixed_offset() - at)
            .to_std()
            .unwrap_or_default();
        Instant::now().checked_sub(ago)
    });
    (spoken, last)
}

/// Append one event to the session's `assistant.jsonl`, so what the assistant
/// said is kept with the meeting and deleted with it.
fn log(dir: &Path, mut event: Value) {
    event["at"] = json!(chrono::Local::now().to_rfc3339());
    let line = event.to_string() + "\n";
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("assistant.jsonl"))
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

/// Whether a heartbeat says an assistant is listening right now: written in
/// the last ten seconds by a process that still exists.
pub fn listening(config_file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(Watch::heartbeat_path(config_file)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let pid = i32::try_from(v["pid"].as_u64()?).ok()?;
    let at = v["updated_at"].as_u64()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    // SAFETY: signal 0 sends nothing; it only asks whether `pid` exists.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    (alive && now.saturating_sub(at) <= 10)
        .then(|| v["state"].as_str().unwrap_or("listening").to_string())
}

/// The `watch` prompt: what an agent is told when the user runs it, e.g. as
/// `/mcp__ambient__watch` in Claude Code. `focus` is whatever the user typed
/// after it.
pub fn watch_prompt(name: &str, focus: Option<&str>) -> String {
    let mut p = format!(
        "You are {name}, an AI assistant taking part in a live meeting by voice, through \
Ambient's MCP tools. You hear the meeting as a transcript and speak through a local voice.

1. Call `watch_meeting` once. It announces out loud that an AI is listening; if it fails, \
tell the user why and stop.
2. Then loop: call `wait_for_transcript`, read the new lines, decide, and call \
`wait_for_transcript` again. Keep looping until it returns `status` `ended` or `off`, or the \
user tells you to stop. Never end your turn while the meeting is live: every turn ends in a \
tool call.
3. Speak only when it clearly helps:
   - someone addresses you by name ({name}) or asks the AI something: answer straight away;
   - a factual question is put to the room and nobody answers it: give the people a chance \
first, and speak only if the next lines show it still open;
   - something clearly wrong is about to be acted on.
   Otherwise stay silent. Most stretches of a meeting need nothing from you.
4. To speak, call `speak` with one to three short sentences, at most about 40 words, the way \
a thoughtful colleague would say them aloud: plain words, no lists, markdown, links or code, \
and no preamble like \"Great question\". Pick the `emotion` that fits. If `speak` is \
refused, keep listening.
5. Between tool calls, write nothing, or at most a few words. The transcript is the meeting; \
do not summarise it back.
6. When `wait_for_transcript` reports the meeting `ended` or `off`, watching is over: give \
the user a two-line summary of what you said, if anything. If the user asks you to stop \
before then, call `stop_watching`."
    );
    if let Some(f) = focus.map(str::trim).filter(|f| !f.is_empty()) {
        p.push_str(&format!("\n\nThe user adds: {f}"));
    }
    p
}

#[cfg(test)]
mod tests;
