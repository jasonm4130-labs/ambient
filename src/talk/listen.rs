//! The `ambient mcp` half of talking: the user's agent session hears what
//! the user said through push-to-talk, and answers it.
//!
//! [`Desk`] is the state behind four MCP tools, one per `ambient mcp`
//! process. `talk_listen` makes this process the listener: it writes the
//! heartbeat the app reads to know an agent is there, and starts a thread
//! that pushes every open question in the [`mailbox`] to the client once, as
//! a Claude Code channel notification:
//!
//! ```text
//! {"jsonrpc":"2.0","method":"notifications/claude/channel",
//!  "params":{"content":"what the user said","meta":{"request_id":"ambient-…","asked_at":"…"}}}
//! ```
//!
//! which Claude Code shows the model as `<channel source="ambient"
//! request_id="…">…</channel>`. A client without channels loops on
//! `wait_for_talk` instead. Either way the model answers with `talk_reply`,
//! and the `ambient talk` worker that posted the question speaks it.
//!
//! The last session to listen wins: the thread asks whom the heartbeat names
//! every tick and before every push, and stops as soon as it names another
//! listener, so only one session ever hears a question. Every write of the
//! heartbeat happens under [`mailbox::with_beat_lock`] with that check, so
//! an older listener never writes over a newer one's.

use super::mailbox::{self, Listener, Mailbox};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Where whole JSON-RPC lines go: the request loop's answers and this
/// module's notifications share it, a line at a time under the lock.
pub type Out = Arc<Mutex<dyn Write + Send>>;

/// A tool that ran and could not do what was asked, worded for the agent.
pub type Refused = String;

/// How often the listener looks for new questions.
const TICK: Duration = Duration::from_millis(250);

/// The heartbeat is rewritten every this many ticks: two seconds.
const BEAT_EVERY: u64 = 8;

/// The mailbox is swept every this many ticks: a minute.
const SWEEP_EVERY: u64 = 240;

/// The longest a single `wait_for_talk` may block, and its default. A
/// client's tool-call timeout must be longer; Claude Code's is.
const MAX_WAIT: Duration = Duration::from_secs(120);
const DEFAULT_WAIT: Duration = Duration::from_secs(60);

/// What `talk_listen` tells the model to do next.
const HOW: &str = "Listening. What the user says out loud arrives in this conversation as \
    <channel source=\"ambient\" request_id=\"…\"> messages; treat each as the user speaking to \
    you here, do what they ask, and answer it with talk_reply. Lead with one to three short \
    sentences meant to be heard; put any detail after a blank line, which is shown and not \
    read. Your transcript output is never heard. If the user says their questions are not \
    reaching you, call wait_for_talk in a loop instead.";

/// The `instructions` an MCP client is given at `initialize`.
pub const INSTRUCTIONS: &str = "Ambient's push-to-talk: after talk_listen (or the talk prompt), \
    what the user says out loud arrives as <channel source=\"ambient\" request_id=\"…\"> \
    messages. Answer each with talk_reply(request_id, text), leading with one to three short \
    spoken sentences; transcript output is never heard.";

/// What the listener thread shares with the tools.
struct Shared {
    /// This listener's id in the heartbeat.
    id: String,
    push: bool,
    /// Questions already pushed by this process, so each goes once.
    pushed: HashSet<String>,
    since: String,
    client: Option<String>,
}

/// The state behind the talking tools, one per `ambient mcp` process.
pub struct Desk {
    config_file: PathBuf,
    out: Out,
    shared: Arc<Mutex<Shared>>,
    /// The running listener thread's own flag: it clears it when another
    /// session takes over, and [`Desk::stop`] clears it to end the thread.
    alive: Option<Arc<AtomicBool>>,
    /// Questions `wait_for_talk` has already returned. Kept apart from
    /// `pushed`: a push to a client without channels is dropped silently, so
    /// it must not stop the same question being collected.
    handed: HashSet<String>,
    tick: Duration,
}

fn lock<T: ?Sized>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Write one JSON-RPC message as one line and flush it.
pub fn send_line(out: &Out, v: &Value) -> std::io::Result<()> {
    let mut w = lock(out);
    writeln!(w, "{v}")?;
    w.flush()
}

/// The channel notification for one question.
pub fn notification(note: &mailbox::Note) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/claude/channel",
        "params": {
            "content": note.text,
            "meta": {"request_id": note.request_id, "asked_at": note.asked_at},
        },
    })
}

/// Whether the heartbeat now names a listener other than `id`: another
/// session listened after this one, and it hears now.
fn superseded(config_file: &std::path::Path, id: &str) -> bool {
    mailbox::last_beat(config_file).is_some_and(|l| l.id != id)
}

/// A fresh id for one listener: the pid and a clock reading, which two
/// listeners in one process never share.
fn listener_id() -> String {
    use std::sync::atomic::AtomicU64;
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{}-{nanos:x}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )
}

impl Desk {
    pub fn new(config_file: PathBuf, out: Out) -> Self {
        Self {
            config_file,
            out,
            shared: Arc::new(Mutex::new(Shared {
                id: listener_id(),
                push: false,
                pushed: HashSet::new(),
                since: String::new(),
                client: None,
            })),
            alive: None,
            handed: HashSet::new(),
            tick: TICK,
        }
    }

    /// A shorter tick, so a test can watch the listener without waiting.
    pub fn with_tick(mut self, tick: Duration) -> Self {
        self.tick = tick;
        self
    }

    /// Note who the client is, from `initialize`'s `clientInfo`, for the
    /// heartbeat the app shows.
    pub fn on_initialize(&mut self, params: Option<&Value>) {
        let name = params
            .and_then(|p| p["clientInfo"]["name"].as_str())
            .map(str::to_string);
        lock(&self.shared).client = name;
    }

    /// The four tools, as `tools/list` reports them.
    pub fn tools() -> Vec<Value> {
        vec![
            json!({
                "name": "talk_listen",
                "description": "Start hearing what the user says to you out loud through \
                    Ambient's push-to-talk. Each question then arrives in this conversation as \
                    a <channel source=\"ambient\" request_id=\"…\"> message (Claude Code started \
                    with --dangerously-load-development-channels server:ambient); answer it with \
                    talk_reply. Without channels, loop on wait_for_talk instead. The last \
                    session to call this is the one that hears.",
                "inputSchema": {"type": "object", "properties": {}},
            }),
            json!({
                "name": "wait_for_talk",
                "description": "Block until the user says something to you out loud through \
                    Ambient's push-to-talk, then return it with its request_id, or a null \
                    request_id after the timeout. For clients without Claude Code channels: \
                    loop on this, answering each question with talk_reply. Makes this session \
                    the one that hears, as talk_listen does.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "timeout_s": {"type": "integer", "minimum": 1, "maximum": MAX_WAIT.as_secs(), "description": "Longest to wait (default 60)."},
                    },
                },
            }),
            json!({
                "name": "talk_reply",
                "description": "Answer a question the user asked out loud through Ambient. \
                    Ambient shows the whole reply as text and reads its first paragraph aloud, \
                    so lead with one to three short spoken sentences in plain words (no \
                    markdown, lists, links or code), and put any detail after a blank line. \
                    Refused if the question expired or was already answered.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "request_id": {"type": "string", "description": "The request_id of the question, from its <channel> tag or from wait_for_talk."},
                        "text": {"type": "string", "description": "The answer."},
                    },
                    "required": ["request_id", "text"],
                },
            }),
            json!({
                "name": "stop_listening",
                "description": "Stop hearing the user's spoken questions in this session.",
                "inputSchema": {"type": "object", "properties": {}},
            }),
        ]
    }

    /// Run one of [`Desk::tools`]; `None` if `name` is not one of them.
    pub fn call(&mut self, name: &str, args: &Value) -> Option<Result<Value, Refused>> {
        Some(match name {
            "talk_listen" => self.listen(true),
            "wait_for_talk" => {
                let timeout = args
                    .get("timeout_s")
                    .and_then(Value::as_u64)
                    .map_or(DEFAULT_WAIT, Duration::from_secs)
                    .clamp(Duration::from_secs(1), MAX_WAIT);
                self.wait(timeout)
            }
            "talk_reply" => self.reply(args),
            "stop_listening" => Ok(self.stop()),
            _ => return None,
        })
    }

    fn mailbox(&self) -> Result<Mailbox, Refused> {
        Mailbox::at(&self.config_file).map_err(|e| format!("Ambient's talk mailbox: {e}"))
    }

    /// Whether this process is the listener right now.
    pub fn listening(&self) -> bool {
        self.alive
            .as_ref()
            .is_some_and(|a| a.load(Ordering::SeqCst))
    }

    /// Become the listener, pushing questions or not.
    fn listen(&mut self, push: bool) -> Result<Value, Refused> {
        let mb = self.mailbox()?;
        mb.sweep();
        let me = std::process::id();
        let id = lock(&self.shared).id.clone();
        let beat = {
            let mut s = lock(&self.shared);
            s.push = push;
            if !self.listening() || s.since.is_empty() {
                s.since = chrono::Local::now().to_rfc3339();
            }
            Listener {
                pid: me,
                id: s.id.clone(),
                since: s.since.clone(),
                push,
                client: s.client.clone(),
            }
        };
        // Under the lock, so the listener this replaces cannot write its own
        // heartbeat back over this one.
        let (replaced, wrote) = mailbox::with_beat_lock(&self.config_file, || {
            let replaced = mailbox::listener(&self.config_file)
                .filter(|l| l.id != id)
                .map(|l| l.pid);
            (replaced, mailbox::write_beat(&self.config_file, &beat))
        });
        wrote.map_err(|e| format!("could not write Ambient's talk heartbeat: {e}"))?;
        if !self.listening() {
            self.start_thread();
        }
        Ok(json!({
            "listening": true,
            "push": push,
            "replaced_pid": replaced,
            "open": mb.open_notes().len(),
            "how": HOW,
        }))
    }

    fn start_thread(&mut self) {
        let alive = Arc::new(AtomicBool::new(true));
        self.alive = Some(Arc::clone(&alive));
        let (cfg, out, shared, tick) = (
            self.config_file.clone(),
            Arc::clone(&self.out),
            Arc::clone(&self.shared),
            self.tick,
        );
        std::thread::spawn(move || {
            let me = std::process::id();
            let id = lock(&shared).id.clone();
            let mut n: u64 = 0;
            while alive.load(Ordering::SeqCst) {
                // Whether this is still the listener is asked every tick, not
                // only at the heartbeat, and under the lock with the
                // heartbeat's refresh: a session that listened after this one
                // hears from its first tick, and a stop or a newer listener
                // can never be written over.
                let still = mailbox::with_beat_lock(&cfg, || {
                    if !alive.load(Ordering::SeqCst) || superseded(&cfg, &id) {
                        return false;
                    }
                    if n % BEAT_EVERY == 0 {
                        let beat = {
                            let s = lock(&shared);
                            Listener {
                                pid: me,
                                id: id.clone(),
                                since: s.since.clone(),
                                push: s.push,
                                client: s.client.clone(),
                            }
                        };
                        let _ = mailbox::write_beat(&cfg, &beat);
                    }
                    true
                });
                if !still {
                    alive.store(false, Ordering::SeqCst);
                    break;
                }
                let mb = Mailbox::at(&cfg).ok();
                if let Some(mb) = mb.as_ref().filter(|_| n % SWEEP_EVERY == SWEEP_EVERY - 1) {
                    mb.sweep();
                }
                let pushing = lock(&shared).push;
                if let Some(mb) = mb.filter(|_| pushing) {
                    for note in mb.open_notes() {
                        if lock(&shared).pushed.contains(&note.request_id) {
                            continue;
                        }
                        // Asked again right before each push. The lock is not
                        // held across the write to stdout, which can block on
                        // a client that has stopped reading.
                        if superseded(&cfg, &id) {
                            alive.store(false, Ordering::SeqCst);
                            return;
                        }
                        lock(&shared).pushed.insert(note.request_id.clone());
                        if send_line(&out, &notification(&note)).is_err() {
                            // The client has gone; nothing can hear now.
                            alive.store(false, Ordering::SeqCst);
                            return;
                        }
                    }
                }
                n += 1;
                std::thread::sleep(tick);
            }
        });
    }

    /// `wait_for_talk`.
    fn wait(&mut self, timeout: Duration) -> Result<Value, Refused> {
        if !self.listening() || lock(&self.shared).push {
            self.listen(false)?;
        }
        let mb = self.mailbox()?;
        let started = Instant::now();
        let id = lock(&self.shared).id.clone();
        loop {
            // Asked before a note is handed over, not after: a session that
            // listened since gets it instead.
            if !self.listening() || superseded(&self.config_file, &id) {
                return Ok(json!({
                    "request_id": null,
                    "listening": false,
                    "note": "Another session is listening now, so questions go there. Stop \
                        looping, unless the user asks you to listen again.",
                }));
            }
            if let Some(note) = mb
                .open_notes()
                .into_iter()
                .find(|n| !self.handed.contains(&n.request_id))
            {
                self.handed.insert(note.request_id.clone());
                return Ok(json!({
                    "request_id": note.request_id,
                    "text": note.text,
                    "asked_s_ago": note.age().as_secs(),
                }));
            }
            if started.elapsed() >= timeout {
                return Ok(json!({
                    "request_id": null,
                    "listening": true,
                    "note": "Nothing was said. Call wait_for_talk again.",
                }));
            }
            std::thread::sleep(self.tick);
        }
    }

    /// `talk_reply`.
    fn reply(&mut self, args: &Value) -> Result<Value, Refused> {
        let rid = args.get("request_id").and_then(Value::as_str).unwrap_or("");
        let text = args.get("text").and_then(Value::as_str).unwrap_or("");
        self.mailbox()?
            .reply(rid, text)
            .map_err(|r| r.to_string())?;
        Ok(json!(
            "Sent. Ambient shows it in its window and reads the first paragraph aloud."
        ))
    }

    /// `stop_listening`.
    pub fn stop(&mut self) -> Value {
        let was = self.listening();
        // A process that never listened has no heartbeat to remove, and
        // need not read the user's.
        if let Some(a) = self.alive.take() {
            let id = lock(&self.shared).id.clone();
            // Under the lock: the thread checks `alive` under it too, so it
            // cannot write the heartbeat back after it is removed here.
            mailbox::with_beat_lock(&self.config_file, || {
                a.store(false, Ordering::SeqCst);
                mailbox::remove_own_beat(&self.config_file, &id);
            });
        }
        json!({"listening": false, "was_listening": was})
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The `talk` prompt: what an agent is told when the user runs it, e.g. as
/// `/mcp__ambient__talk` in Claude Code. With `wait`, for a client without
/// channels, it loops on `wait_for_talk`.
pub fn talk_prompt(wait: bool) -> String {
    let answer = "Answer with `talk_reply`, passing the question's `request_id`. Lead with one \
to three short sentences meant to be heard: plain words, no markdown, lists, links or code, and \
no preamble. Put any detail after a blank line; Ambient shows it but does not read it out. Your \
transcript output is never heard, so a question answered only in the chat goes unanswered. If \
`talk_reply` says the question expired, do not retry; mention the answer in the chat if it \
still matters.";
    let intro = "The user talks to you out loud through Ambient: they hold a key, speak, and \
Ambient transcribes it on their Mac and hands you the words. Ambient reads your answer aloud.";
    if wait {
        format!(
            "{intro}

1. Call `wait_for_talk`. When it returns a `request_id`, treat the `text` as the user speaking \
to you in this conversation and do what they ask, using tools as usual.
2. {answer}
3. Then call `wait_for_talk` again. Keep looping until the user tells you to stop; when it \
returns a null `request_id`, just call it again. Between calls, write nothing.
4. If it says another session is listening now, stop looping. When the user tells you to stop, \
call `stop_listening`."
        )
    } else {
        format!(
            "{intro}

1. Call `talk_listen` once. Then tell the user in one short line that you are listening, and \
end your turn.
2. Each thing they say arrives in this conversation as a `<channel source=\"ambient\" \
request_id=\"…\">` message. Treat it as the user speaking to you here and do what they ask, \
using tools as usual.
3. {answer}
4. Questions reach you this way only when Claude Code was started with \
`--dangerously-load-development-channels server:ambient`. If the user says their questions are \
not arriving, call `wait_for_talk` in a loop instead, answering each with `talk_reply`."
        )
    }
}

#[cfg(test)]
#[path = "listen_tests.rs"]
mod tests;
