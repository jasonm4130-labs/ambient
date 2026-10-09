//! The app's half of talking to firstmate: the hold, the turns, the words.
//!
//! The menu bar owns one [`Talk`]. A press of ⌥Space (or the window's talk
//! button) opens the microphone into memory with [`crate::capture::MicHold`];
//! the release hands those samples to a fresh `ambient talk --pcm` child
//! ([`crate::talk`]) over its stdin and forgets them. The child's JSON events
//! come back here and become [`Turn`]s, which the menu, the status icon and
//! the window's talk panel are all drawn from.
//!
//! Turns are kept in memory only, and only the last [`MAX_TURNS`]: firstmate's
//! inbox already keeps the record, and Ambient keeps no talk log of its own.

use crate::config::TalkConfig;
use crate::state::PhaseKind;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

/// Turns the window and menu remember.
pub const MAX_TURNS: usize = 20;

/// A hold shorter than this was a slip of the key; it is dropped without
/// starting the worker.
pub const MIN_HOLD_S: f64 = 0.3;

/// How long a refusal or a one-off message stays in the menu and window.
const NOTICE_FOR: Duration = Duration::from_secs(10);

/// The status icon while the microphone is held open for a turn.
pub const LISTENING_SYMBOL: &str = "mic.fill";
/// The status icon while a reply is being read aloud.
pub const SPEAKING_SYMBOL: &str = "speaker.wave.2.fill";
/// Every symbol this module can put in the menu bar, for `symbolcheck`.
pub const SYMBOLS: [&str; 2] = [LISTENING_SYMBOL, SPEAKING_SYMBOL];

/// Where one turn has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The worker is trimming and transcribing the audio.
    Hearing,
    /// The transcript is going to firstmate.
    Sending,
    /// The note is with firstmate; no reply yet.
    Waiting,
    /// The reply is being read aloud.
    Speaking,
    /// The reply arrived, and was said or shown.
    Done,
    /// Too little speech; nothing was sent.
    Nothing,
    /// The worker stopped waiting before firstmate answered.
    NoReply,
    /// Stopped by the user, or by a recording starting.
    Stopped,
    Failed,
}

impl Stage {
    fn word(self) -> &'static str {
        match self {
            Stage::Hearing => "hearing",
            Stage::Sending => "sending",
            Stage::Waiting => "waiting",
            Stage::Speaking => "speaking",
            Stage::Done => "done",
            Stage::Nothing => "nothing",
            Stage::NoReply => "no_reply",
            Stage::Stopped => "stopped",
            Stage::Failed => "failed",
        }
    }

    /// Whether the worker still has something to do.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            Stage::Hearing | Stage::Sending | Stage::Waiting | Stage::Speaking
        )
    }
}

/// One hold and what came of it: both sides as text.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub id: u64,
    /// What the user said, once transcribed.
    pub you: Option<String>,
    /// firstmate's reply, as it wrote it.
    pub reply: Option<String>,
    pub stage: Stage,
    /// Why a turn is queued, text only, stopped or failed.
    pub note: Option<String>,
    /// Whether the reply is to be read aloud. Off, the worker runs with
    /// `--dry-run` and the reply is only shown.
    speak: bool,
}

impl Turn {
    pub fn new(id: u64, speak: bool) -> Self {
        Self {
            id,
            you: None,
            reply: None,
            stage: Stage::Hearing,
            note: None,
            speak,
        }
    }

    /// Fold one of the worker's stdout events (see [`crate::talk`]) in.
    pub fn apply(&mut self, event: &Value) {
        let text = |k: &str| event[k].as_str().map(str::to_string);
        match event["event"].as_str().unwrap_or_default() {
            "heard" => {
                self.you = text("text");
                self.stage = Stage::Sending;
            }
            "nothing" => {
                self.stage = Stage::Nothing;
                self.note = Some("Didn't catch that, so nothing was sent.".into());
            }
            "queued" => {
                self.note = Some(format!(
                    "Queued: {}",
                    text("reason").unwrap_or_else(|| "firstmate is busy".into())
                ));
            }
            "sent" => {
                self.stage = Stage::Waiting;
                if event["announced"] == json!(false) {
                    self.note = Some(
                        "Saved, but firstmate was not woken; it will see it at its next check."
                            .into(),
                    );
                }
            }
            "reply" => {
                self.reply = text("text");
                self.note = None;
                self.stage = if self.speak {
                    Stage::Speaking
                } else {
                    Stage::Done
                };
            }
            "spoken" | "dry_run" => self.stage = Stage::Done,
            "text_only" => {
                self.stage = Stage::Done;
                self.note = text("reason").map(|r| format!("Not said aloud: {r}"));
            }
            "no_reply" => {
                self.stage = Stage::NoReply;
                self.note =
                    Some("firstmate has not replied yet; its reply will be in its inbox.".into());
            }
            _ => {}
        }
    }

    /// The worker has exited. A turn it left open was stopped (by a signal)
    /// or failed (with what it printed on stderr).
    pub fn exited(&mut self, code: Option<i32>, stderr: &str) {
        if !self.stage.is_open() {
            return;
        }
        // 130 is the worker's own SIGTERM handler; no code is a signal that
        // got there before the handler was installed.
        if code == Some(130) || code.is_none() {
            self.stage = Stage::Stopped;
            self.note = Some(if self.reply.is_some() {
                "Stopped.".into()
            } else {
                "Stopped before firstmate replied; its reply will be in its inbox.".into()
            });
            return;
        }
        self.stage = Stage::Failed;
        let why = stderr
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("the talk worker exited without saying why");
        let why = why.strip_prefix("Error: ").unwrap_or(why);
        self.note = Some(why.to_string());
    }

    fn payload(&self) -> Value {
        json!({
            "id": self.id,
            "you": self.you,
            "reply": self.reply,
            "state": self.stage.word(),
            "note": self.note,
        })
    }
}

/// Why a press is refused, or `None` to start listening.
///
/// The microphone is shared with recording, so talking is paused while a
/// recording runs or a call is waiting to be recorded: an aside would land in
/// the meeting's transcript, and a spoken reply would be heard by everyone
/// in it.
pub fn refusal(cfg: &TalkConfig, phase: PhaseKind) -> Option<&'static str> {
    if !cfg.enabled {
        return Some("Talking mode is off. Turn it on in Settings.");
    }
    if let Some(why) = pause_reason(phase) {
        return Some(why);
    }
    if cfg.firstmate_home.is_none() {
        return Some("Choose firstmate's home in Settings first.");
    }
    None
}

/// Why talking is paused in this phase, as the menu and the window say it.
pub fn pause_reason(phase: PhaseKind) -> Option<&'static str> {
    match phase {
        PhaseKind::Recording | PhaseKind::Stopping => {
            Some("Talking is paused while Ambient is recording.")
        }
        PhaseKind::Armed => Some("Talking is paused while a call is waiting to be recorded."),
        PhaseKind::Idle | PhaseKind::Failed => None,
    }
}

/// Why talking is paused right now, for the window's button.
pub fn paused(phase: PhaseKind) -> Option<&'static str> {
    match phase {
        PhaseKind::Recording | PhaseKind::Stopping => Some("recording"),
        PhaseKind::Armed => Some("armed"),
        PhaseKind::Idle | PhaseKind::Failed => None,
    }
}

/// What a worker sends back to the main thread.
enum Msg {
    Event(u64, Value),
    Exit(u64, Option<i32>, String),
}

/// Every turn, the workers behind the open ones, and the hold in progress.
pub struct Talk {
    turns: VecDeque<Turn>,
    /// Worker pids by turn, for barge-in. Removed when the worker exits.
    pids: HashMap<u64, u32>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    next_id: u64,
    /// The microphone, open while the key is down.
    pub hold: Option<crate::capture::MicHold>,
    notice: Option<(String, Instant)>,
}

impl Default for Talk {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            turns: VecDeque::new(),
            pids: HashMap::new(),
            tx,
            rx,
            next_id: 1,
            hold: None,
            notice: None,
        }
    }
}

impl Talk {
    pub fn listening(&self) -> bool {
        self.hold.is_some()
    }

    pub fn speaking(&self) -> bool {
        self.turns.iter().any(|t| t.stage == Stage::Speaking)
    }

    /// The status icon this state calls for, if it overrides the phase's.
    pub fn symbol(&self) -> Option<&'static str> {
        if self.listening() {
            Some(LISTENING_SYMBOL)
        } else if self.speaking() {
            Some(SPEAKING_SYMBOL)
        } else {
            None
        }
    }

    /// Say something in the menu and the window for a few seconds.
    pub fn notice(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now()));
    }

    fn current_notice(&self) -> Option<&str> {
        self.notice
            .as_ref()
            .filter(|(_, at)| at.elapsed() < NOTICE_FOR)
            .map(|(t, _)| t.as_str())
    }

    /// Start a turn: a worker that hears `samples` (mono at `rate`), sends
    /// them to firstmate and waits for the reply. Returns the turn's id.
    pub fn spawn(&mut self, samples: Vec<f32>, rate: f64, speak: bool) -> anyhow::Result<u64> {
        let exe = std::env::current_exe()?;
        let mut cmd = Command::new(exe);
        cmd.args([
            super::SUBCOMMAND,
            "--pcm",
            "--rate",
            &(rate.round() as u32).to_string(),
        ]);
        if !speak {
            cmd.arg("--dry-run");
        }
        // A hush that lands before the worker has its handler is ignored
        // rather than fatal; the next tick sends it again.
        // SAFETY: signal is async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc::signal(libc::SIGUSR1, libc::SIG_IGN);
                Ok(())
            });
        }
        let id = self.start(cmd, pcm_bytes(&samples), speak)?;
        Ok(id)
    }

    /// The process half of [`Talk::spawn`], with the command given, so a test
    /// can stand a script in for the worker.
    fn start(&mut self, mut cmd: Command, stdin: Vec<u8>, speak: bool) -> anyhow::Result<u64> {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let id = self.next_id;
        self.next_id += 1;
        self.pids.insert(id, child.id());
        let mut pipe = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        // The audio is written from its own thread: a minute of 48 kHz audio
        // is more than a pipe holds, and the main thread runs the menu.
        std::thread::spawn(move || {
            let _ = pipe.write_all(&stdin);
        });
        let err = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            s
        });
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if tx.send(Msg::Event(id, v)).is_err() {
                        break;
                    }
                }
            }
            let code = child.wait().ok().and_then(|s| s.code());
            let stderr = err.join().unwrap_or_default();
            let _ = tx.send(Msg::Exit(id, code, stderr));
        });
        self.turns.push_back(Turn::new(id, speak));
        while self.turns.len() > MAX_TURNS {
            self.turns.pop_front();
        }
        Ok(id)
    }

    /// Take in whatever the workers have said. Returns whether anything
    /// changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            changed = true;
            match msg {
                Msg::Event(id, v) => {
                    if let Some(t) = self.turns.iter_mut().find(|t| t.id == id) {
                        t.apply(&v);
                    }
                }
                Msg::Exit(id, code, stderr) => {
                    self.pids.remove(&id);
                    if let Some(t) = self.turns.iter_mut().find(|t| t.id == id) {
                        t.exited(code, &stderr);
                    }
                }
            }
        }
        changed
    }

    /// Silence whatever is being said: holding the key again interrupts.
    pub fn barge_in(&mut self) {
        let ids: Vec<u64> = self
            .turns
            .iter()
            .filter(|t| t.stage == Stage::Speaking)
            .map(|t| t.id)
            .collect();
        for id in ids {
            self.signal(id, libc::SIGTERM);
        }
    }

    /// Keep every open turn quiet: a recording started or a call is waiting
    /// to be recorded. A reply already being said stops; one still to come
    /// arrives as text only.
    pub fn hush(&mut self) {
        let ids: Vec<u64> = self.pids.keys().copied().collect();
        for id in ids {
            self.signal(id, libc::SIGUSR1);
        }
    }

    /// Stop every worker: the app is quitting.
    pub fn stop_all(&mut self) {
        let ids: Vec<u64> = self.pids.keys().copied().collect();
        for id in ids {
            self.signal(id, libc::SIGTERM);
        }
    }

    fn signal(&mut self, id: u64, sig: libc::c_int) {
        if let Some(&pid) = self.pids.get(&id) {
            // SAFETY: plain syscall to our own child, which has not been
            // reaped yet (its pid leaves the map when the reaper reports).
            unsafe { libc::kill(pid as libc::pid_t, sig) };
        }
    }

    /// The menu's one talk line: a notice, the hold, or the latest turn.
    pub fn menu_line(&self) -> Option<String> {
        if let Some(n) = self.current_notice() {
            return Some(n.to_string());
        }
        if self.listening() {
            return Some(format!(
                "Listening… let go of {} to send",
                crate::hotkey::CHORD
            ));
        }
        let t = self.turns.back()?;
        Some(match t.stage {
            Stage::Hearing => "Working out what you said…".into(),
            Stage::Sending => format!("Sending “{}”", short(t.you.as_deref().unwrap_or(""))),
            Stage::Waiting => "Waiting for firstmate's reply…".into(),
            Stage::Speaking | Stage::Done => match &t.reply {
                Some(r) => format!("firstmate: {}", short(r)),
                None => "Done".into(),
            },
            Stage::Nothing => "Didn't catch that".into(),
            Stage::NoReply => "No reply from firstmate yet".into(),
            Stage::Stopped => match &t.reply {
                Some(r) => format!("firstmate: {}", short(r)),
                None => "Stopped".into(),
            },
            Stage::Failed => format!(
                "Talk failed: {}",
                short(t.note.as_deref().unwrap_or("unknown"))
            ),
        })
    }

    /// The window's `talk` event.
    pub fn payload(&self, cfg: &TalkConfig, hotkey: Result<(), &str>, phase: PhaseKind) -> Value {
        json!({
            "enabled": cfg.enabled,
            "speak": cfg.speak,
            "configured": cfg.firstmate_home.is_some(),
            "chord": crate::hotkey::CHORD,
            "hotkey_error": hotkey.err(),
            "listening": self.listening(),
            "paused": paused(phase),
            "notice": self.current_notice(),
            "turns": self.turns.iter().map(Turn::payload).collect::<Vec<_>>(),
        })
    }
}

/// One line of a reply for the menu: the first line, as plain words, cut to
/// fit.
fn short(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    super::cap(&super::speakable(line), 70)
}

/// Mono samples as the worker's `--pcm` reads them: 32-bit float LE.
fn pcm_bytes(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
