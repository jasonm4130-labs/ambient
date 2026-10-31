//! Where a spoken question waits for the user's own agent, and where its
//! answer comes back.
//!
//! `ambient talk` posts what the user said as a note in `talk/`, beside the
//! config file. The `ambient mcp` process that is listening in the user's
//! agent session (see [`super::listen`]) hands the note to the agent, and the
//! agent's `talk_reply` writes the answer next to it. The worker reads the
//! answer, speaks it, and removes both files. Nothing here is a log: a note
//! lives only as long as its question is open.
//!
//! ```text
//! talk/<rid>.json         {"request_id","text","asked_at","worker_pid","timeout_s"}  the worker, tmp then rename
//! talk/<rid>.reply.json   {"text","at","pid"}                           talk_reply, first one wins
//! talk/<rid>.expired      empty                                         the worker, when it gives up
//! talk.json               {"pid","id","since","updated_at","push","client"}  the listener's heartbeat
//! ```
//!
//! A note is **open** while it has no reply and no expired mark, the worker
//! that posted it is still running, and it is younger than the worker's
//! timeout. The running-worker rule is what makes barge-in safe: a worker
//! stopped by SIGTERM leaves through `_exit`, so it never cleans up, and its
//! note simply stops being open.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The folder beside the config file.
pub const DIR: &str = "talk";

/// How long the listener's heartbeat counts as fresh. It is rewritten every
/// two seconds.
pub const BEAT_FRESH: Duration = Duration::from_secs(10);

/// How long an expired mark is kept, so a late `talk_reply` is told the
/// question expired rather than that it never existed.
const EXPIRED_KEPT: Duration = Duration::from_secs(3600);

/// How long a staged file or an orphaned reply may sit before a sweep takes
/// it: long enough that a write in progress is never caught.
const LEFTOVER: Duration = Duration::from_secs(60);

/// One question, as the worker posted it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub request_id: String,
    pub text: String,
    /// RFC 3339.
    pub asked_at: String,
    pub worker_pid: u32,
    /// How long the worker waits for the answer.
    pub timeout_s: u64,
}

impl Note {
    /// Seconds since it was asked; a clock that went backwards reads as now.
    pub fn age(&self) -> Duration {
        chrono::DateTime::parse_from_rfc3339(&self.asked_at)
            .ok()
            .and_then(|at| (chrono::Local::now().fixed_offset() - at).to_std().ok())
            .unwrap_or_default()
    }
}

/// Why `talk_reply` did not take an answer. Worded for the agent, which reads
/// it as the tool's error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyRefused {
    /// Not an id Ambient hands out; nothing on disk was touched.
    BadId,
    Empty,
    /// No note by that id.
    Unknown,
    /// The worker stopped waiting, or was stopped.
    Expired,
    /// Another reply got there first.
    Answered,
}

impl std::fmt::Display for ReplyRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ReplyRefused::BadId => {
                "That is not a request_id Ambient gave out. Copy the request_id from the \
                 <channel source=\"ambient\"> tag or from wait_for_talk."
            }
            ReplyRefused::Empty => "The reply is empty. Say something, in plain sentences.",
            ReplyRefused::Unknown => {
                "There is no question with that request_id. It may have been answered and \
                 cleared already; do not retry."
            }
            ReplyRefused::Expired => {
                "That question expired: the user stopped waiting for the spoken answer. \
                 Do not retry; if it still matters, say it in the chat instead."
            }
            ReplyRefused::Answered => "That question has already been answered. Do not retry.",
        })
    }
}

/// Whether `rid` is shaped like the ids [`super::request_id`] makes:
/// `ambient-` and 24 lowercase hex digits. Checked before any path is built
/// from it, because it arrives from the agent.
pub fn valid_id(rid: &str) -> bool {
    rid.strip_prefix("ambient-").is_some_and(|hex| {
        hex.len() == 24
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Whether a process with this pid exists.
pub(crate) fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 sends nothing; it only asks whether `pid` exists.
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

fn modified_ago(path: &Path) -> Option<Duration> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
}

/// Write `bytes` to `path` whole or not at all: staged under a name a listing
/// skips, then renamed.
fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = staged(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(bytes)?;
    drop(f);
    std::fs::rename(&tmp, path)
}

/// `.<name>.<pid>.tmp` beside `path`: hidden from [`Mailbox::open_notes`],
/// and per process so two writers never share one.
fn staged(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

/// The `talk/` folder.
#[derive(Debug, Clone)]
pub struct Mailbox {
    dir: PathBuf,
}

impl Mailbox {
    /// The mailbox beside `config_file`, created owner-only if it is not
    /// there yet.
    pub fn at(config_file: &Path) -> std::io::Result<Self> {
        let dir = config_file.with_file_name(DIR);
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn note_path(&self, rid: &str) -> PathBuf {
        self.dir.join(format!("{rid}.json"))
    }

    fn reply_path(&self, rid: &str) -> PathBuf {
        self.dir.join(format!("{rid}.reply.json"))
    }

    fn expired_path(&self, rid: &str) -> PathBuf {
        self.dir.join(format!("{rid}.expired"))
    }

    /// Post a question from the worker `worker_pid`, which waits `timeout`.
    pub fn post(
        &self,
        rid: &str,
        text: &str,
        worker_pid: u32,
        timeout: Duration,
    ) -> std::io::Result<Note> {
        let note = Note {
            request_id: rid.to_string(),
            text: text.to_string(),
            asked_at: chrono::Local::now().to_rfc3339(),
            worker_pid,
            timeout_s: timeout.as_secs(),
        };
        self.write_note(&note)?;
        Ok(note)
    }

    pub(crate) fn write_note(&self, note: &Note) -> std::io::Result<()> {
        if !valid_id(&note.request_id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{:?} is not a talk request id", note.request_id),
            ));
        }
        let body = serde_json::to_vec(note).map_err(std::io::Error::other)?;
        write_whole(&self.note_path(&note.request_id), &body)
    }

    /// The note by `rid`, open or not.
    pub fn note(&self, rid: &str) -> Option<Note> {
        if !valid_id(rid) {
            return None;
        }
        let text = std::fs::read_to_string(self.note_path(rid)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Whether `note` is still waiting for its answer.
    pub fn is_open(&self, note: &Note) -> bool {
        let rid = &note.request_id;
        !self.reply_path(rid).exists()
            && !self.expired_path(rid).exists()
            && alive(note.worker_pid)
            && note.age() < Duration::from_secs(note.timeout_s)
    }

    /// Every note under `talk/`, oldest first.
    fn notes(&self) -> Vec<Note> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut notes: Vec<Note> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let rid = name.strip_suffix(".json")?;
                // `<rid>.reply.json` strips to `<rid>.reply`, which is no id.
                valid_id(rid).then(|| self.note(rid)).flatten()
            })
            .collect();
        notes.sort_by(|a, b| {
            let at = |n: &Note| chrono::DateTime::parse_from_rfc3339(&n.asked_at).ok();
            at(a)
                .cmp(&at(b))
                .then_with(|| a.request_id.cmp(&b.request_id))
        });
        notes
    }

    /// The questions still waiting for an answer, oldest first.
    pub fn open_notes(&self) -> Vec<Note> {
        self.notes()
            .into_iter()
            .filter(|n| self.is_open(n))
            .collect()
    }

    /// Answer `rid`. The first answer wins; every other outcome is refused
    /// with the reason, and a refused answer touches nothing.
    pub fn reply(&self, rid: &str, text: &str) -> Result<(), ReplyRefused> {
        if !valid_id(rid) {
            return Err(ReplyRefused::BadId);
        }
        if text.trim().is_empty() {
            return Err(ReplyRefused::Empty);
        }
        if self.expired_path(rid).exists() {
            return Err(ReplyRefused::Expired);
        }
        if self.reply_path(rid).exists() {
            return Err(ReplyRefused::Answered);
        }
        let note = self.note(rid).ok_or(ReplyRefused::Unknown)?;
        if !self.is_open(&note) {
            return Err(ReplyRefused::Expired);
        }
        let body = json!({
            "text": text,
            "at": chrono::Local::now().to_rfc3339(),
            "pid": std::process::id(),
        });
        // Staged whole, then linked into place: a link fails if the name is
        // taken, so a second answer cannot replace the first, and the worker
        // never reads half of one.
        let target = self.reply_path(rid);
        let tmp = staged(&target);
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .and_then(|mut f| f.write_all(body.to_string().as_bytes()));
        let linked = written.and_then(|()| std::fs::hard_link(&tmp, &target));
        let _ = std::fs::remove_file(&tmp);
        match linked {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(ReplyRefused::Answered),
            // A disk that will not take the file is not the agent's fault,
            // but it is the same advice: the answer will not be heard.
            Err(_) => Err(ReplyRefused::Expired),
        }
    }

    /// The answer to `rid`, if it has come. Taking it clears the question:
    /// the note and the answer are both removed.
    pub fn take_reply(&self, rid: &str) -> Option<String> {
        if !valid_id(rid) {
            return None;
        }
        let text = std::fs::read_to_string(self.reply_path(rid)).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        let reply = v["text"].as_str()?.to_string();
        self.finish(rid);
        Some(reply)
    }

    /// The worker stopped waiting: mark it, so a late answer is told so, and
    /// drop the question's text.
    pub fn expire(&self, rid: &str) {
        if !valid_id(rid) {
            return;
        }
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(self.expired_path(rid));
        self.finish(rid);
    }

    /// Remove the note and its answer.
    pub fn finish(&self, rid: &str) {
        if !valid_id(rid) {
            return;
        }
        let _ = std::fs::remove_file(self.note_path(rid));
        let _ = std::fs::remove_file(self.reply_path(rid));
    }

    /// Clear what nobody will read: notes whose worker is gone or whose wait
    /// is long over, answers whose note is gone, old expired marks and staged
    /// files left by a crash.
    pub fn sweep(&self) {
        let grace = LEFTOVER.as_secs();
        for note in self.notes() {
            let over = note.age() >= Duration::from_secs(note.timeout_s.saturating_add(grace));
            if !alive(note.worker_pid) || over {
                self.finish(&note.request_id);
            }
        }
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for e in entries.filter_map(Result::ok) {
            let path = e.path();
            let Some(name) = e.file_name().into_string().ok() else {
                continue;
            };
            let ago = modified_ago(&path).unwrap_or_default();
            let stale = if let Some(rid) = name.strip_suffix(".reply.json") {
                !self.note_path(rid).exists() && ago >= LEFTOVER
            } else if name.ends_with(".expired") {
                ago >= EXPIRED_KEPT
            } else {
                name.starts_with('.') && name.ends_with(".tmp") && ago >= LEFTOVER
            };
            if stale {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// The `ambient mcp` process listening for questions, from its heartbeat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Listener {
    pub pid: u32,
    /// Which listener in that process: the heartbeat is rewritten only by
    /// the listener it names, so the last one to start listening wins.
    #[serde(default)]
    pub id: String,
    /// RFC 3339: when it started listening.
    pub since: String,
    /// Whether questions are pushed to it as channel messages (`talk_listen`)
    /// rather than collected with `wait_for_talk`.
    pub push: bool,
    /// The MCP client's own name for itself, from `initialize`.
    pub client: Option<String>,
}

/// The heartbeat file, beside the config file.
pub fn beat_path(config_file: &Path) -> PathBuf {
    config_file.with_file_name("talk.json")
}

/// Whether an agent is listening right now: a heartbeat written in the last
/// [`BEAT_FRESH`] by a process that still exists.
pub fn listener(config_file: &Path) -> Option<Listener> {
    let text = std::fs::read_to_string(beat_path(config_file)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let at = v["updated_at"].as_u64()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let l: Listener = serde_json::from_value(v).ok()?;
    (alive(l.pid) && now.saturating_sub(at) <= BEAT_FRESH.as_secs()).then_some(l)
}

/// The listener the heartbeat names, fresh or not.
pub fn last_beat(config_file: &Path) -> Option<Listener> {
    let text = std::fs::read_to_string(beat_path(config_file)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write `l` as the heartbeat, stamped now.
pub fn write_beat(config_file: &Path, l: &Listener) -> std::io::Result<()> {
    let mut v = serde_json::to_value(l).map_err(std::io::Error::other)?;
    v["updated_at"] = json!(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()));
    write_whole(&beat_path(config_file), v.to_string().as_bytes())
}

/// Remove the heartbeat only if listener `id` in this process wrote it:
/// another session may be listening now.
pub fn remove_own_beat(config_file: &Path, id: &str) {
    if last_beat(config_file).is_some_and(|l| l.pid == std::process::id() && l.id == id) {
        let _ = std::fs::remove_file(beat_path(config_file));
    }
}

#[cfg(test)]
#[path = "mailbox_tests.rs"]
mod tests;
