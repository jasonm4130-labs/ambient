//! `ambient assist` — the live assistant: it follows the meeting being
//! recorded, decides when it has something worth saying, and says it.
//!
//! It is its own process, never part of the recorder. It reads the live
//! session through [`api::call`], the same dispatcher `ambient mcp` serves,
//! with the same `transcript` cursor an outside MCP client would poll. Each
//! new stretch of transcript goes to a cheap jump-in model ("should I speak?");
//! on a confident yes that the [`gate::Gate`] allows, a stronger reply model
//! writes one to three spoken sentences, and the voice helper says them
//! ([`voice::Voice`]).
//!
//! Consent comes first. The assistant is off unless `assistant` is turned on,
//! and on joining a meeting it says out loud that an AI is listening and may
//! speak before it does anything else. While it listens it keeps a heartbeat
//! file the menu bar reads, so "AI assistant listening" is on screen too.

pub mod gate;
pub mod llm;
pub mod voice;

use crate::api::{self, Paths};
use crate::config::{AssistantConfig, Config};
use anyhow::{anyhow, Result};
use gate::{is_echo, Gate, Hold, Rules, Verdict};
use llm::{Model, Reply, Request};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How many recent lines the models see. Enough for the thread of a
/// conversation, little enough to keep the jump-in call cheap.
const CONTEXT_LINES: usize = 40;

/// How long the assistant remembers what it said, for echo detection.
const ECHO_WINDOW: Duration = Duration::from_secs(120);

/// The least time between two jump-in questions.
const MIN_INTERVAL: Duration = Duration::from_secs(4);

/// Said once on joining each meeting, before anything else.
pub fn consent_notice(name: &str) -> String {
    format!(
        "Hi everyone. Just so you know, an AI assistant called {name} is listening to this \
         meeting and may speak up."
    )
}

/// Where the voice goes. [`voice::Voice`] in a real run; tests record instead.
pub trait Speaker {
    fn say(&mut self, text: &str, emotion: &str) -> Result<()>;
    /// Called every poll, so a helper can be stopped when idle.
    fn tick(&mut self) {}
    fn stop(&mut self) {}
}

impl Speaker for voice::Voice {
    fn say(&mut self, text: &str, emotion: &str) -> Result<()> {
        let spoken = voice::Voice::say(self, text, emotion)?;
        eprintln!(
            "  voice: {:.1}s of audio, first sound after {} ms",
            spoken.audio_s,
            spoken
                .first_audio_ms
                .map_or_else(|| "?".to_string(), |ms| ms.to_string())
        );
        Ok(())
    }
    fn tick(&mut self) {
        voice::Voice::tick(self)
    }
    fn stop(&mut self) {
        voice::Voice::stop(self)
    }
}

/// Prints instead of speaking: `ambient assist --silent`.
pub struct Printer;

impl Speaker for Printer {
    fn say(&mut self, text: &str, emotion: &str) -> Result<()> {
        println!("[{emotion}] {text}");
        Ok(())
    }
}

/// One line of the meeting, as the models see it.
#[derive(Debug, Clone)]
struct Heard {
    at_ms: u64,
    who: String,
    text: String,
}

/// What the assistant knows about the meeting it is in.
struct Meeting {
    id: String,
    dir: PathBuf,
    cursor: u64,
    context: VecDeque<Heard>,
    gate: Gate,
    /// What it said and when, for echo detection.
    said: Vec<(Instant, String)>,
    /// New words from a person since the jump-in model was last asked.
    pending: bool,
}

/// What one poll did, for the caller's logging and for tests.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Off,
    NoMeeting,
    Joined(String),
    Quiet,
    Held(String),
    Spoke(String),
}

pub struct Assistant<'a> {
    paths: Paths,
    /// Follow this session rather than whichever one is live.
    only: Option<String>,
    model: &'a dyn Model,
    speaker: &'a mut dyn Speaker,
    meeting: Option<Meeting>,
}

impl<'a> Assistant<'a> {
    pub fn new(
        paths: Paths,
        only: Option<String>,
        model: &'a dyn Model,
        speaker: &'a mut dyn Speaker,
    ) -> Self {
        Self {
            paths,
            only,
            model,
            speaker,
            meeting: None,
        }
    }

    /// The heartbeat file, beside the config file.
    pub fn heartbeat_path(config_file: &Path) -> PathBuf {
        config_file.with_file_name("assistant.json")
    }

    fn live_session(&self) -> Option<String> {
        let status = api::call("status", &json!({}), &self.paths).ok()?;
        let live = status["live"]["id"].as_str()?.to_string();
        match &self.only {
            Some(id) if *id != live => None,
            _ => Some(live),
        }
    }

    fn transcript(&self, id: &str, since: u64) -> Result<(u64, Vec<Value>)> {
        let v = api::call(
            "transcript",
            &json!({"session": id, "since": since}),
            &self.paths,
        )
        .map_err(|e| anyhow!("reading {id}: {e:?}"))?;
        let next = v["next"].as_u64().unwrap_or(since);
        let lines = v["lines"].as_array().cloned().unwrap_or_default();
        Ok((next, lines))
    }

    /// One poll: notice the meeting starting or ending, read what is new, and
    /// maybe speak. `now` is passed in so tests control the clock.
    pub fn step(&mut self, now: Instant) -> Result<Step> {
        let cfg = Config::load_from(&self.paths.config_file).assistant;
        if !cfg.enabled {
            self.leave("the assistant was turned off");
            return Ok(Step::Off);
        }
        let Some(live) = self.live_session() else {
            self.leave("the meeting ended");
            return Ok(Step::NoMeeting);
        };
        if self.meeting.as_ref().map(|m| m.id.as_str()) != Some(live.as_str()) {
            self.leave("a new meeting started");
            self.join(&live, &cfg, now)?;
            return Ok(Step::Joined(live));
        }
        self.beat("listening");
        self.read_new(now)?;
        let step = self.consider(&cfg, now)?;
        self.speaker.tick();
        Ok(step)
    }

    fn join(&mut self, id: &str, cfg: &AssistantConfig, now: Instant) -> Result<()> {
        let (next, lines) = self.transcript(id, 0)?;
        let dir = self.paths.root().join(id);
        let mut meeting = Meeting {
            id: id.to_string(),
            dir,
            cursor: next,
            context: VecDeque::new(),
            gate: Gate::new(Rules {
                threshold: cfg.threshold,
                cooldown: Duration::from_secs(cfg.cooldown_s.into()),
                max_per_meeting: cfg.max_per_meeting,
                min_interval: MIN_INTERVAL,
            }),
            said: Vec::new(),
            // What was said before it joined is context, not a cue.
            pending: false,
        };
        for line in lines.iter().rev().take(CONTEXT_LINES).rev() {
            meeting.context.push_back(heard(line));
        }
        eprintln!("assistant: joined {id} — announcing that an AI is listening");
        let notice = consent_notice(&cfg.name);
        meeting.said.push((now, notice.clone()));
        log(&meeting.dir, json!({"event": "joined", "notice": notice}));
        self.meeting = Some(meeting);
        self.beat("listening");
        if let Err(e) = self.speaker.say(&notice, "warm") {
            // The on-screen notice still stands; say why the spoken one did not.
            eprintln!("assistant: could not speak the notice: {e:#}");
        }
        Ok(())
    }

    fn leave(&mut self, why: &str) {
        if let Some(m) = self.meeting.take() {
            eprintln!("assistant: left {} — {why}", m.id);
            log(
                &m.dir,
                json!({"event": "left", "why": why, "spoke": m.gate.spoken()}),
            );
            self.speaker.stop();
        }
        let _ = std::fs::remove_file(Self::heartbeat_path(&self.paths.config_file));
    }

    fn read_new(&mut self, now: Instant) -> Result<()> {
        let Some(m) = self.meeting.as_ref() else {
            return Ok(());
        };
        let (next, lines) = self.transcript(&m.id, m.cursor)?;
        let m = self.meeting.as_mut().expect("checked above");
        m.cursor = next;
        m.said
            .retain(|(t, _)| now.saturating_duration_since(*t) < ECHO_WINDOW);
        let recent: Vec<&str> = m.said.iter().map(|(_, s)| s.as_str()).collect();
        for line in &lines {
            let mut h = heard(line);
            if is_echo(&h.text, &recent) {
                h.who = "(the assistant, heard back)".into();
            } else {
                m.pending = true;
            }
            m.context.push_back(h);
            while m.context.len() > CONTEXT_LINES {
                m.context.pop_front();
            }
        }
        Ok(())
    }

    fn consider(&mut self, cfg: &AssistantConfig, now: Instant) -> Result<Step> {
        let Some(m) = self.meeting.as_mut() else {
            return Ok(Step::Quiet);
        };
        if !m.pending {
            return Ok(Step::Quiet);
        }
        match m.gate.may_ask(now) {
            Ok(()) => {}
            // Asked moments ago: keep the new lines pending for the next poll.
            Err(h @ Hold::TooSoon(_)) => return Ok(Step::Held(h.to_string())),
            Err(h) => {
                m.pending = false;
                return Ok(Step::Held(h.to_string()));
            }
        }
        m.gate.asked(now);
        m.pending = false;
        let transcript = render(&m.context);
        let asked = self.model.complete(&Request {
            model: cfg.jump_in_model.clone(),
            system: llm::jump_in_system(&cfg.name),
            user: format!(
                "The meeting so far, oldest first; the newest lines are last:\n\n{transcript}\n\n\
                 Should {} speak now?",
                cfg.name
            ),
            max_tokens: 80,
            timeout: Duration::from_secs(15),
        });
        let m = self.meeting.as_mut().expect("still in the meeting");
        let asked = match asked {
            Ok(c) => c,
            Err(e) => {
                eprintln!("assistant: jump-in call failed: {e:#}");
                log(
                    &m.dir,
                    json!({"event": "error", "stage": "jump-in", "message": format!("{e:#}")}),
                );
                return Ok(Step::Held("the jump-in call failed".into()));
            }
        };
        let verdict: Verdict = llm::parse_verdict(&asked.text);
        log(
            &m.dir,
            json!({
                "event": "jump-in", "model": cfg.jump_in_model, "speak": verdict.speak,
                "confidence": verdict.confidence, "reason": verdict.reason,
                "latency_ms": asked.latency.as_millis() as u64, "cost_usd": asked.cost,
            }),
        );
        if let Err(h) = m.gate.judge(&verdict, now) {
            return Ok(Step::Held(h.to_string()));
        }
        eprintln!(
            "assistant: jumping in ({:.2}: {}) after {} ms",
            verdict.confidence,
            verdict.reason,
            asked.latency.as_millis()
        );
        let written = self.model.complete(&Request {
            model: cfg.reply_model.clone(),
            system: llm::reply_system(&cfg.name),
            user: format!(
                "The meeting so far, oldest first:\n\n{transcript}\n\n\
                 You were brought in because: {}\n\nWhat do you say?",
                verdict.reason
            ),
            max_tokens: 200,
            timeout: Duration::from_secs(30),
        });
        let m = self.meeting.as_mut().expect("still in the meeting");
        let written = match written {
            Ok(c) => c,
            Err(e) => {
                eprintln!("assistant: reply call failed: {e:#}");
                log(
                    &m.dir,
                    json!({"event": "error", "stage": "reply", "message": format!("{e:#}")}),
                );
                return Ok(Step::Held("the reply call failed".into()));
            }
        };
        let Some(Reply { text, emotion }) = llm::parse_reply(&written.text) else {
            log(
                &m.dir,
                json!({"event": "error", "stage": "reply", "message": "unusable reply"}),
            );
            return Ok(Step::Held("the reply was unusable".into()));
        };
        log(
            &m.dir,
            json!({
                "event": "reply", "model": cfg.reply_model, "text": text, "emotion": emotion,
                "latency_ms": written.latency.as_millis() as u64, "cost_usd": written.cost,
            }),
        );
        // Counted as spoken before the voice runs: a failed voice still
        // printed the reply, and retrying it later would be stale.
        m.gate.spoke(now);
        m.said.push((now, text.clone()));
        m.context.push_back(Heard {
            at_ms: m.context.back().map_or(0, |h| h.at_ms),
            who: format!("{} (you)", cfg.name),
            text: text.clone(),
        });
        eprintln!("assistant says [{emotion}]: {text}");
        self.beat("speaking");
        if let Err(e) = self.speaker.say(&text, &emotion) {
            eprintln!("assistant: could not speak: {e:#}");
        }
        self.beat("listening");
        Ok(Step::Spoke(text))
    }

    /// Write the heartbeat the menu bar reads to show the listening notice.
    fn beat(&self, state: &str) {
        let Some(m) = &self.meeting else { return };
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let beat = json!({
            "pid": std::process::id(),
            "session": m.id,
            "state": state,
            "updated_at": at,
        });
        let path = Self::heartbeat_path(&self.paths.config_file);
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, beat.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Leave the meeting and remove the heartbeat. Called on exit.
    pub fn shutdown(&mut self) {
        self.leave("the assistant stopped");
    }
}

fn heard(line: &Value) -> Heard {
    let who = match line["speaker"].as_str() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => match line["track"].as_str() {
            Some("call") => "someone on the call".into(),
            _ => "someone in the room".into(),
        },
    };
    Heard {
        at_ms: line["start_ms"].as_u64().unwrap_or(0),
        who,
        text: line["text"].as_str().unwrap_or("").trim().to_string(),
    }
}

/// The context as the models read it: `[mm:ss] who: text`, one per line.
fn render(context: &VecDeque<Heard>) -> String {
    context
        .iter()
        .map(|h| {
            let s = h.at_ms / 1000;
            format!("[{:02}:{:02}] {}: {}", s / 60, s % 60, h.who, h.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Append one event to the session's `assistant.jsonl`, so what the assistant
/// decided and said is kept with the meeting and deleted with it.
fn log(dir: &Path, mut event: Value) {
    let at = chrono::Local::now().to_rfc3339();
    event["at"] = json!(at);
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
    let text = std::fs::read_to_string(Assistant::heartbeat_path(config_file)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let pid = i32::try_from(v["pid"].as_u64()?).ok()?;
    let at = v["updated_at"].as_u64()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    // SAFETY: signal 0 sends nothing; it only asks whether `pid` exists.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    (alive && now.saturating_sub(at) <= 10)
        .then(|| v["state"].as_str().unwrap_or("listening").to_string())
}

/// The options `ambient assist` takes.
#[derive(Debug, Default)]
pub struct Options {
    pub session: Option<String>,
    /// Print replies instead of speaking them.
    pub silent: bool,
    /// Extra arguments for the voice helper, e.g. `--no-play`.
    pub voice_args: Vec<String>,
}

/// `ambient assist`: run until interrupted.
pub fn run(opts: Options) -> Result<()> {
    let config_file = crate::config::path();
    let cfg = Config::load_from(&config_file);
    let model = llm::OpenRouter::from_env(&cfg.assistant.base_url, cfg.assistant.zdr)?;
    let mut speaker: Box<dyn Speaker> = if opts.silent {
        Box::new(Printer)
    } else {
        let engine = voice::resolve_engine(cfg.voice.engine, voice::total_ram());
        let dir = cfg
            .voice
            .helper_dir
            .clone()
            .unwrap_or_else(voice::default_helper_dir);
        let (program, args) = voice::helper_command(&dir, engine, &cfg.voice, &opts.voice_args);
        eprintln!(
            "assistant: voice {engine} ({} GB of memory here), helper in {}",
            voice::total_ram() >> 30,
            dir.display()
        );
        let mut v = voice::Voice::new(program, args, voice::Limits::default());
        // Load the model now, so the first reply is not also its cold start.
        v.warm()?;
        Box::new(v)
    };
    if !cfg.assistant.enabled {
        eprintln!(
            "assistant: off. Turn it on from the menu bar (Live Assistant) or with \
             `ambient config assistant on`; waiting."
        );
    }
    eprintln!(
        "assistant: deciding with {}, replying with {}; threshold {}, cooldown {}s, at most {} \
         per meeting",
        cfg.assistant.jump_in_model,
        cfg.assistant.reply_model,
        cfg.assistant.threshold,
        cfg.assistant.cooldown_s,
        cfg.assistant.max_per_meeting
    );
    let paths = Paths {
        config_file,
        roster_file: crate::roster::path(),
        sessions_root: Some(crate::session::home()),
    };
    let mut assistant = Assistant::new(paths, opts.session, &model, speaker.as_mut());
    let stop = interrupted();
    let mut last = Step::Off;
    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
        match assistant.step(Instant::now()) {
            Ok(step) => {
                // A hold is worth a line when it is news; "asking again in …"
                // is the loop pacing itself and never is.
                if let (Step::Held(why), false) = (&step, step == last) {
                    if !why.starts_with("asking again") {
                        eprintln!("assistant: quiet — {why}");
                    }
                }
                last = step;
            }
            Err(e) => eprintln!("assistant: {e:#}"),
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    assistant.shutdown();
    Ok(())
}

/// A flag set by Ctrl-C or SIGTERM, so the loop can leave the meeting cleanly
/// and remove the heartbeat rather than leaving a stale "listening" notice.
fn interrupted() -> &'static std::sync::atomic::AtomicBool {
    use std::sync::atomic::AtomicBool;
    static STOP: AtomicBool = AtomicBool::new(false);
    extern "C" fn on_signal(_: libc::c_int) {
        STOP.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    // SAFETY: the handler only stores to an atomic, which is signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    &STOP
}

#[cfg(test)]
mod tests;
