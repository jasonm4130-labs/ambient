//! The one dispatcher every session verb goes through — `ambient mcp` today,
//! and later the window's bridge.
//!
//! A method name and a JSON `params` value in, a JSON `Value` or an
//! [`ApiError`] out. Keeping this signature frozen (see the plan's
//! Non-goals) is what lets `ambient mcp` and the window call the same code
//! without either one growing a special case for the other.

use crate::config::Config;
use crate::session;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// A call that never ran (the client's bug, mapped to JSON-RPC `-32602`) or
/// one that ran and could not answer (mapped to a tool result with
/// `isError: true`). The distinction is what lets a caller retry a `Failed`
/// with different arguments and know a `InvalidParams` needs a different
/// shape instead.
#[derive(Debug)]
pub enum ApiError {
    InvalidParams(String),
    Failed(String),
}

/// One entry of [`methods`]: a name, a human description and a JSON Schema
/// for its arguments, the same shape `tools/list` reports over MCP.
pub struct Method {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

/// Every method this dispatcher answers, in the order `ambient mcp`'s
/// `tools/list` reports them. Descriptions and schemas are unchanged from
/// what `mcp::tools()` hardcoded before this module existed.
pub fn methods() -> Vec<Method> {
    vec![
        Method {
            name: "sessions",
            description: "Every recorded session on this machine, newest first.",
            input_schema: json!({"type": "object", "properties": {}}),
        },
        Method {
            name: "transcript",
            description: "The lines of one session, including one being recorded now.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session": {"type": "string", "description": "The session id, as `sessions` reports it."},
                    "since": {"type": "integer", "description": "Skip this many lines; pass back the previous reply's `next`."},
                    "verbatim": {"type": "boolean", "description": "Return the words as recognised, before any naming edits."},
                },
                "required": ["session"],
            }),
        },
        Method {
            name: "status",
            description:
                "What Ambient is doing right now: the live session, if any, and what is waiting.",
            input_schema: json!({"type": "object", "properties": {}}),
        },
    ]
}

/// The files and folders every `api::call` reads and writes through, rather
/// than a global default: a test builds one pointed entirely at temp files,
/// and the real process builds one pointed at the user's.
pub struct Paths {
    pub config_file: PathBuf,
    pub roster_file: PathBuf,
    /// Set only when the caller handed a sessions root over explicitly
    /// (`mcp::serve`'s `root` argument, or `AMBIENT_HOME` for the real
    /// process). Anything else re-reads `sessions_dir` from the config file
    /// on every call, so a `config.set` of it moves the very next call.
    pub sessions_root: Option<PathBuf>,
}

impl Paths {
    /// The sessions root for this call, read fresh every time: a pinned
    /// `sessions_root` wins, otherwise today's config file, otherwise the
    /// `~/Documents/Ambient` default — mirroring [`session::home`]'s tail
    /// without going through it, since a test must never call that function.
    pub fn root(&self) -> PathBuf {
        if let Some(root) = &self.sessions_root {
            return root.clone();
        }
        if let Some(dir) = Config::load_from(&self.config_file).sessions_dir {
            return dir;
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join("Documents").join("Ambient")
    }
}

/// The dispatcher. `mcp::call` and (later) the window's bridge both route
/// through this one function; its signature is frozen once this unit lands,
/// so later methods are new match arms here, never new arguments.
pub fn call(method: &str, params: &Value, paths: &Paths) -> Result<Value, ApiError> {
    match method {
        "status" => Ok(status(&paths.root())),
        "sessions" => Ok(sessions(&paths.root())),
        "transcript" => transcript(&paths.root(), params),
        other => Err(ApiError::InvalidParams(format!("no such method {other:?}"))),
    }
}

/// The `sessions` method: every session directory under the root, newest
/// first.
///
/// Enumerated by [`session_dirs`] rather than by [`session::summaries`], which
/// walks with `is_dir` and so would summarise — and read the `session.json` of
/// — whatever a link in the folder points at. A session whose files include a
/// symlink is listed with `error` set and no metadata: it is on this machine
/// and a person should see it, but nothing behind that link is read.
fn sessions(root: &Path) -> Value {
    let listed: Vec<Value> = session_dirs(root)
        .iter()
        .rev()
        .filter_map(|dir| match symlinked(dir) {
            Some(refusal) => Some(session::SessionSummary {
                id: dir.file_name()?.to_string_lossy().into_owned(),
                dir: dir.clone(),
                name: None,
                started_at: None,
                duration_s: None,
                transcribed: dir.join("transcript.md").is_file(),
                live: session::is_growing(&dir.join("audio").join("room.native.wav")),
                transcribing: session::live_transcriber(dir).is_some(),
                error: Some(refusal),
            }),
            None => session::summarise(dir),
        })
        .filter_map(|s| serde_json::to_value(s).ok())
        .collect();
    Value::Array(listed)
}

/// The `transcript` method. Every argument's JSON type is checked here,
/// before any of them reaches the filesystem: a wrong type is the client's
/// bug and an `InvalidParams`, while a session that cannot be read is a call
/// that ran and could not answer.
fn transcript(root: &Path, params: &Value) -> Result<Value, ApiError> {
    let args = params
        .as_object()
        .ok_or_else(|| ApiError::InvalidParams("`transcript` needs a string `session`".into()))?;
    let id = args
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidParams("`session` must be a string".into()))?;
    let since = match args.get("since") {
        None => 0,
        Some(v) => v.as_u64().ok_or_else(|| {
            ApiError::InvalidParams("`since` must be a whole number of lines".into())
        })?,
    };
    let verbatim = match args.get("verbatim") {
        None => false,
        Some(v) => v
            .as_bool()
            .ok_or_else(|| ApiError::InvalidParams("`verbatim` must be true or false".into()))?,
    };
    read(root, id, since, verbatim).map_err(ApiError::Failed)
}

/// One session from `since` lines on, or the reason it could not be read.
///
/// The cursor counts lines in the order `raw.jsonl` holds them, which is why
/// this reads [`session::transcript_appended`] and not [`session::transcript`]:
/// the room track is written before the call track and their clocks are
/// independent, so a cursor on `start_ms` would step past call lines that
/// arrived later and never show them. A session with no `raw.jsonl` yet is not
/// a failure — it is a recording that has produced no words so far, and it
/// answers with an empty list and a `next` of 0.
fn read(root: &Path, id: &str, since: u64, verbatim: bool) -> Result<Value, String> {
    let dir = session_dir(root, id)?;
    if let Some(refusal) = symlinked(&dir) {
        return Err(refusal);
    }
    let lines = if dir.join("raw.jsonl").exists() {
        session::transcript_appended(&dir, verbatim).map_err(|e| format!("{e:#}"))?
    } else {
        Vec::new()
    };
    let next = lines.len() as u64;
    let seen = usize::try_from(since).unwrap_or(usize::MAX);
    let unseen: Vec<session::Line> = lines.into_iter().skip(seen).collect();
    Ok(json!({"session": id, "state": state(&dir), "next": next, "lines": unseen}))
}

/// `root/id`, or why that is not a session of this machine. An id is one path
/// segment: a separator or a `..` in it is a request for a path outside the
/// sessions folder, and is answered before anything is opened. The directory
/// itself is tested with `symlink_metadata`, so a link in the folder is no
/// more a session here than it is to [`session_dirs`].
fn session_dir(root: &Path, id: &str) -> Result<PathBuf, String> {
    if id.is_empty() || id == "." || id == ".." || id.contains('/') {
        return Err(format!("{id:?} is not a session id"));
    }
    let dir = root.join(id);
    if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
        return Err(format!("no session {id}"));
    }
    Ok(dir)
}

/// The one word for what is happening to a session, in the order it is
/// happening: a capture in flight outranks a transcriber, which outranks the
/// `transcript.md` a previous run left. `pending` is the rest — captured, and
/// waiting for the queue.
fn state(dir: &Path) -> &'static str {
    if session::is_growing(&dir.join("audio").join("room.native.wav")) {
        "live"
    } else if session::live_transcriber(dir).is_some() {
        "transcribing"
    } else if dir.join("transcript.md").is_file() {
        "done"
    } else {
        "pending"
    }
}

/// The files a method reads out of a session directory.
const READ_FROM_SESSIONS: [&str; 3] = ["session.json", "raw.jsonl", "edits.jsonl"];

/// The refusal for the first of those files that exists and is not a regular
/// file. Containment is the whole point: the directory being inside the root
/// says nothing about where a file inside it points, and following one would
/// serve a file from anywhere on disk as a session of this machine.
fn symlinked(dir: &Path) -> Option<String> {
    READ_FROM_SESSIONS
        .iter()
        .map(|name| dir.join(name))
        .find(|p| std::fs::symlink_metadata(p).is_ok_and(|m| !m.is_file()))
        .map(|p| format!("refusing symlink {}", p.display()))
}

/// Every field of `status` is drawn from [`session_dirs`], and none of them
/// from anywhere else.
///
/// `session::live_session_in` and `session::captured_awaiting_transcript` both
/// enumerate the root with `is_dir`, which follows a symlink: left to
/// themselves they will happily report a directory somewhere else on disk as a
/// session of this machine. `live` repeats `live_session_in`'s rule here — the
/// last id whose native scratch wav is still growing — rather than filtering
/// its answer, because a link sorting after the real live session would
/// otherwise not merely be excluded but hide it.
fn status(root: &Path) -> Value {
    let dirs = session_dirs(root);
    let live = dirs
        .iter()
        .rev()
        .find(|d| session::is_growing(&d.join("audio").join("room.native.wav")))
        .and_then(|d| d.file_name().map(|n| json!({"id": n.to_string_lossy()})));
    let awaiting = session::captured_awaiting_transcript(root)
        .into_iter()
        .filter(|d| dirs.contains(d))
        .count();
    json!({
        "live": live,
        "awaiting_transcript": awaiting,
        "sessions": dirs.len(),
    })
}

/// The directories under `root` that are sessions, sorted by id.
///
/// A directory counts when it holds a `session.json` or a growing native
/// scratch wav — the two ends of a recording's life — which is what keeps a
/// stray `notes` folder out of the count. Unlike [`session::list`] the test is
/// `symlink_metadata`, so a link in the sessions folder is not a session
/// whatever it points at. This is the server's containment, which is why
/// [`status`] answers every one of its fields from this set, and why both
/// `sessions` and `status.sessions` count it and so cannot disagree.
fn session_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()))
        .filter(|p| {
            p.join("session.json").is_file()
                || session::is_growing(&p.join("audio").join("room.native.wav"))
        })
        .collect();
    dirs.sort();
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roster;

    /// A `Paths` pointed entirely at temp files unique to this test — never
    /// the user's real config, roster or sessions folder.
    fn temp_paths(test: &str) -> (Paths, PathBuf) {
        let base = std::env::temp_dir().join(format!("ambient-{test}-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        std::fs::create_dir_all(&base).unwrap();
        let root = base.join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        let config_file = base.join("config.json");
        Config {
            sessions_dir: Some(root.clone()),
            ..Default::default()
        }
        .save_to(&config_file)
        .unwrap();
        let roster_file = base.join("roster.json");
        (
            Paths {
                config_file,
                roster_file,
                sessions_root: None,
            },
            root,
        )
    }

    #[test]
    fn methods_lists_the_three_methods() {
        let names: Vec<&str> = methods().iter().map(|m| m.name).collect();
        assert_eq!(names, ["sessions", "transcript", "status"]);
    }

    #[test]
    fn status_counts_the_session_directories() {
        let (paths, root) = temp_paths("status");
        let dir = root.join("2026-09-05-1200");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("session.json"), "{}").unwrap();
        std::fs::write(dir.join("transcript.md"), "# transcript\n").unwrap();

        let got = call("status", &json!({}), &paths).unwrap();
        assert_eq!(
            got,
            json!({"live": null, "awaiting_transcript": 0, "sessions": 1})
        );
    }

    #[test]
    fn transcript_since_must_be_a_whole_number() {
        let (paths, _root) = temp_paths("since-type");
        let err = call("transcript", &json!({"session": "x", "since": "1"}), &paths).unwrap_err();
        assert!(matches!(err, ApiError::InvalidParams(_)), "{err:?}");
    }

    #[test]
    fn an_unknown_method_is_invalid_params_naming_it() {
        let (paths, _root) = temp_paths("unknown");
        let err = call("nope", &json!({}), &paths).unwrap_err();
        match err {
            ApiError::InvalidParams(m) => assert!(m.contains("nope"), "{m}"),
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    /// Exercises the `roster_file` field so it is not dead code ahead of the
    /// unit that reads and writes through it.
    #[test]
    fn paths_carries_a_roster_file_untouched_here() {
        let (paths, _root) = temp_paths("roster-field");
        assert!(!paths.roster_file.exists());
        assert!(roster::path() != paths.roster_file);
    }
}
