//! The one dispatcher every session verb goes through — `ambient mcp` today,
//! and later the window's bridge.
//!
//! A method name and a JSON `params` value in, a JSON `Value` or an
//! [`ApiError`] out. Keeping this signature frozen (see the plan's
//! Non-goals) is what lets `ambient mcp` and the window call the same code
//! without either one growing a special case for the other.

use crate::config::Config;
use crate::export;
use crate::roster;
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

/// Every method `ambient mcp`'s `tools/list` advertises as an MCP tool, in
/// that order — not every method [`call`] answers. `search` is a `call` match
/// arm without an entry here: `mcp.rs` selects its three tools out of this
/// list by name, and `search` is deliberately dispatcher-only, reachable from
/// the CLI and the window but not offered to an MCP client. Descriptions and
/// schemas here are unchanged from what `mcp::tools()` hardcoded before this
/// module existed.
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
        "search" => search(&paths.root(), params),
        "session.update" => session_update(&paths.root(), params),
        "session.delete" => session_delete(&paths.root(), params),
        "export" => export_session(&paths.root(), params),
        "config.get" => Ok(config_get(paths)),
        "config.set" => config_set(paths, params),
        "roster.list" => Ok(roster_list(paths)),
        "roster.add" => roster_add(paths, params),
        "roster.remove" => roster_remove(paths, params),
        "speakers.name" => speakers_name(paths, params),
        "speakers.undo" => speakers_undo(paths, params),
        "speakers.unnamed" => speakers_unnamed(paths, params),
        "devices" => Ok(json!({"devices": input_device_names()})),
        "doctor" => Ok(doctor_checks(paths)),
        other => Err(ApiError::InvalidParams(format!("no such method {other:?}"))),
    }
}

/// The ten checks `ambient doctor` prints, as JSON, for the window's Welcome
/// page. The paths come from `Paths` rather than `config::path()` /
/// `session::home()`, so a test points them at temp files.
fn doctor_checks(paths: &Paths) -> Value {
    json!(crate::doctor::run(
        session::models_root(),
        &paths.config_file,
        &paths.root(),
    ))
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
                tags: Vec::new(),
                pinned: false,
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

/// The `search` method. `query` is required and must be a string; `limit`
/// defaults to 50 and, when given, must be a whole number. Argument shape is
/// checked here, the same way [`transcript`] does it, before anything reaches
/// [`session::search`].
fn search(root: &Path, params: &Value) -> Result<Value, ApiError> {
    let args = params
        .as_object()
        .ok_or_else(|| ApiError::InvalidParams("`search` needs a string `query`".into()))?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidParams("`query` must be a string".into()))?;
    let limit = match args.get("limit") {
        None => 50,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| ApiError::InvalidParams("`limit` must be a whole number".into()))?
            as usize,
    };
    let hits =
        session::search(root, query, limit).map_err(|e| ApiError::Failed(format!("{e:#}")))?;
    serde_json::to_value(hits).map_err(|e| ApiError::Failed(format!("{e}")))
}

/// The `session.update` method: rename, tag, note or pin a session by
/// rewriting its `session.json`. `session` is required and must be a string;
/// everything else in `params` is [`session::MetaPatch`], deserialised
/// straight out of the same object so a client can send `session` and the
/// patch fields together in one request.
///
/// Resolved through [`session_dir`] and [`symlinked`] — the same containment
/// [`read`] uses — before anything is written, so a write path is never
/// weaker than the read path next to it. The lock is claimed once, here at
/// the entry point: [`session::update_meta`] stays lock-free so the
/// transcriber can call it (and its siblings) while already holding the lock
/// it took for itself.
fn session_update(root: &Path, params: &Value) -> Result<Value, ApiError> {
    let args = params.as_object().ok_or_else(|| {
        ApiError::InvalidParams("`session.update` needs a string `session`".into())
    })?;
    let id = args
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidParams("`session` must be a string".into()))?;
    let patch: session::MetaPatch = serde_json::from_value(params.clone())
        .map_err(|e| ApiError::InvalidParams(format!("{e}")))?;

    let dir = session_dir(root, id).map_err(ApiError::Failed)?;
    if let Some(refusal) = symlinked(&dir) {
        return Err(ApiError::Failed(refusal));
    }

    let _lock =
        session::claim_transcription(&dir).map_err(|e| ApiError::Failed(format!("{e:#}")))?;
    let meta =
        session::update_meta(&dir, &patch).map_err(|e| ApiError::Failed(format!("{e:#}")))?;
    serde_json::to_value(meta).map_err(|e| ApiError::Failed(format!("{e}")))
}

/// The `session.delete` method: remove a session directory and its audio.
/// `session` is required and must be a string. The window shows its own
/// confirm dialog before calling this; the API does not ask again.
///
/// Resolved through [`session_dir`] and [`symlinked`], the same containment
/// [`read`] and [`session_update`] use, before the lock is claimed once here
/// at the entry point: [`session::delete`] stays lock-free so it joins the
/// same protocol every other writer here follows.
fn session_delete(root: &Path, params: &Value) -> Result<Value, ApiError> {
    let args = params.as_object().ok_or_else(|| {
        ApiError::InvalidParams("`session.delete` needs a string `session`".into())
    })?;
    let id = args
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidParams("`session` must be a string".into()))?;

    let dir = session_dir(root, id).map_err(ApiError::Failed)?;
    if let Some(refusal) = symlinked(&dir) {
        return Err(ApiError::Failed(refusal));
    }

    let lock =
        session::claim_transcription(&dir).map_err(|e| ApiError::Failed(format!("{e:#}")))?;
    session::delete(root, id, &lock).map_err(|e| ApiError::Failed(format!("{e:#}")))?;
    Ok(json!({"session": id, "deleted": true}))
}

/// The `export` method: a session rendered in one of `export::Format`'s six
/// shapes. `session` is required and must be a string; `format` is optional
/// and defaults to `"markdown"`, and a name `export::Format` does not
/// recognise is `InvalidParams` naming all six. Dispatcher-only, the same
/// posture as [`search`] — not in [`methods`] and not offered over MCP.
///
/// Resolved through [`session_dir`] and [`symlinked`], the same containment
/// every other method here uses. No lock: export only reads, and the
/// transcriber never needs to exclude a reader.
fn export_session(root: &Path, params: &Value) -> Result<Value, ApiError> {
    let args = params
        .as_object()
        .ok_or_else(|| ApiError::InvalidParams("`export` needs a string `session`".into()))?;
    let id = args
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidParams("`session` must be a string".into()))?;
    let format_name = match args.get("format") {
        None => "markdown",
        Some(v) => v
            .as_str()
            .ok_or_else(|| ApiError::InvalidParams("`format` must be a string".into()))?,
    };
    let format: export::Format = format_name
        .parse()
        .map_err(|e| ApiError::InvalidParams(format!("{e}")))?;

    let dir = session_dir(root, id).map_err(ApiError::Failed)?;
    if let Some(refusal) = symlinked(&dir) {
        return Err(ApiError::Failed(refusal));
    }

    let text = export::render(&dir, format).map_err(|e| ApiError::Failed(format!("{e:#}")))?;
    Ok(json!({"session": id, "format": format_name, "text": text}))
}

/// Input device names, the shape both `config.get`'s `devices` field and the
/// `devices` method want. `capture::input_devices()` hits real CoreAudio, so
/// no test may assert its contents — only that the shape is an array.
fn input_device_names() -> Vec<String> {
    crate::capture::input_devices()
        .into_iter()
        .map(|(_, name)| name)
        .collect()
}

/// The `config.get` method: the settings payload the window's Settings page
/// receives, built the same way [`crate::settings`]'s `push` does but reading
/// only through `paths` — never `Config::load`, `roster::load` or
/// `session::home()`, all of which reach the user's real files.
fn config_get(paths: &Paths) -> Value {
    let cfg = Config::load_from(&paths.config_file);
    let home = std::env::var("HOME").unwrap_or_default();
    let latest = session::latest(&paths.root());
    json!({
        "apps": cfg.apps,
        "input_device": cfg.input_device,
        "diarize": cfg.diarize,
        "threshold": cfg.threshold,
        "sessions_dir": cfg.sessions_dir,
        "devices": input_device_names(),
        "default_dir": format!("{home}/Documents/Ambient"),
        "ask_before_recording": cfg.ask_before_recording,
        "audio_retention": match cfg.audio_retention_days {
            None => "forever".to_string(),
            Some(n) => n.to_string(),
        },
        "roster": roster::load_from(&paths.roster_file),
        "latest_session": latest
            .as_deref()
            .and_then(|d| d.file_name())
            .map(|n| n.to_string_lossy().to_string()),
    })
}

/// The `config.set` method: change one setting and hand back the fresh
/// `config.get` payload. Refuses a `sessions_dir` change while a session is
/// recording before anything is loaded, and refuses an unknown key before
/// anything is saved — nothing is written on either refusal.
fn config_set(paths: &Paths, params: &Value) -> Result<Value, ApiError> {
    let args = params.as_object().ok_or_else(|| {
        ApiError::InvalidParams("`config.set` needs a string `key` and `value`".into())
    })?;
    let key = args
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidParams("`key` must be a string".into()))?;
    let value = args
        .get("value")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidParams("`value` must be a string".into()))?;

    let live = session::live_session_in(&paths.root());
    crate::config::refuse_while_live(key, live.as_deref())
        .map_err(|e| ApiError::Failed(format!("{e:#}")))?;

    let mut cfg = Config::load_from(&paths.config_file);
    cfg.set(key, value)
        .map_err(|e| ApiError::InvalidParams(format!("{e:#}")))?;
    cfg.save_to(&paths.config_file)
        .map_err(|e| ApiError::Failed(format!("{e:#}")))?;
    Ok(config_get(paths))
}

/// The `roster.list` method: every name on the roster.
fn roster_list(paths: &Paths) -> Value {
    json!(roster::load_from(&paths.roster_file))
}

/// The `roster.add` method: add a name, then answer the roster.
fn roster_add(paths: &Paths, params: &Value) -> Result<Value, ApiError> {
    let name = params
        .as_object()
        .and_then(|o| o.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidParams("`name` must be a string".into()))?;
    let mut names = roster::load_from(&paths.roster_file);
    roster::add(&mut names, name);
    roster::save_to(&paths.roster_file, &names).map_err(|e| ApiError::Failed(format!("{e:#}")))?;
    Ok(json!(names))
}

/// The `roster.remove` method: remove a name, then answer the roster.
fn roster_remove(paths: &Paths, params: &Value) -> Result<Value, ApiError> {
    let name = params
        .as_object()
        .and_then(|o| o.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidParams("`name` must be a string".into()))?;
    let mut names = roster::load_from(&paths.roster_file);
    roster::remove(&mut names, name);
    roster::save_to(&paths.roster_file, &names).map_err(|e| ApiError::Failed(format!("{e:#}")))?;
    Ok(json!(names))
}

/// The `speakers.name` method: give every line labelled `label` the name
/// `name`. Writes `edits.jsonl`, so the entry point claims the lock around
/// the call the same way [`session_update`] does.
fn speakers_name(paths: &Paths, params: &Value) -> Result<Value, ApiError> {
    let args = params.as_object().ok_or_else(|| {
        ApiError::InvalidParams("`speakers.name` needs `session`, `label` and `name`".into())
    })?;
    let id = args
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidParams("`session` must be a string".into()))?;
    let label = args
        .get("label")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidParams("`label` must be a string".into()))?;
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidParams("`name` must be a string".into()))?;

    let dir = session_dir(&paths.root(), id).map_err(ApiError::Failed)?;
    if let Some(refusal) = symlinked(&dir) {
        return Err(ApiError::Failed(refusal));
    }
    let _lock =
        session::claim_transcription(&dir).map_err(|e| ApiError::Failed(format!("{e:#}")))?;
    let renamed =
        session::name_speaker(&dir, label, name).map_err(|e| ApiError::Failed(format!("{e:#}")))?;
    Ok(json!({"renamed": renamed}))
}

/// The `speakers.undo` method: take back the newest batch of names a person
/// typed for one session. Writes `edits.jsonl`, locked the same way
/// [`speakers_name`] is.
fn speakers_undo(paths: &Paths, params: &Value) -> Result<Value, ApiError> {
    let id = params
        .as_object()
        .and_then(|o| o.get("session"))
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidParams("`session` must be a string".into()))?;

    let dir = session_dir(&paths.root(), id).map_err(ApiError::Failed)?;
    if let Some(refusal) = symlinked(&dir) {
        return Err(ApiError::Failed(refusal));
    }
    let _lock =
        session::claim_transcription(&dir).map_err(|e| ApiError::Failed(format!("{e:#}")))?;
    let reverted =
        session::undo_last_naming(&dir).map_err(|e| ApiError::Failed(format!("{e:#}")))?;
    Ok(json!({"reverted": reverted}))
}

/// The `speakers.unnamed` method: speaker labels nobody has named yet, each
/// with the first thing that voice said. Read-only, so no lock, but resolved
/// through the same [`session_dir`]/[`symlinked`] containment as every other
/// session-taking method.
fn speakers_unnamed(paths: &Paths, params: &Value) -> Result<Value, ApiError> {
    let id = params
        .as_object()
        .and_then(|o| o.get("session"))
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::InvalidParams("`session` must be a string".into()))?;

    let dir = session_dir(&paths.root(), id).map_err(ApiError::Failed)?;
    if let Some(refusal) = symlinked(&dir) {
        return Err(ApiError::Failed(refusal));
    }
    let labels = session::unnamed_labels(&dir).map_err(|e| ApiError::Failed(format!("{e:#}")))?;
    Ok(json!(labels
        .into_iter()
        .map(|(label, sample)| json!({"label": label, "sample": sample}))
        .collect::<Vec<_>>()))
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

/// `api::call("doctor", …)` answers the checks `doctor::run` reports, with
/// the paths from `Paths` — a free test function rather than one inside
/// `mod tests`, so `cargo test api::doctor` matches it.
#[cfg(test)]
#[test]
fn doctor_answers_the_checks_doctor_run_reports() {
    let dir = std::env::temp_dir().join(format!("ambient-api-doctor-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let sessions_root = dir.join("sessions");
    std::fs::create_dir_all(&sessions_root).unwrap();
    let paths = Paths {
        config_file: dir.join("config.json"),
        roster_file: dir.join("roster.json"),
        sessions_root: Some(sessions_root.clone()),
    };

    let got = call("doctor", &json!({}), &paths).unwrap();
    let checks = got.as_array().expect("doctor answers an array");
    assert_eq!(checks.len(), 10);

    for c in checks {
        let obj = c.as_object().expect("each check is an object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["detail", "name", "ok"]);
        assert!(obj["name"].is_string());
        assert!(obj["detail"].is_string());
        assert!(obj["ok"].is_boolean());
    }

    let got_names: Vec<&str> = checks.iter().map(|c| c["name"].as_str().unwrap()).collect();
    let expected = crate::doctor::run(session::models_root(), &paths.config_file, &paths.root());
    let expected_names: Vec<&str> = expected.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(got_names, expected_names);

    let config = checks.iter().find(|c| c["name"] == "config").unwrap();
    assert_eq!(config["ok"], true);
    assert_eq!(config["detail"], "defaults");

    let writable = checks
        .iter()
        .find(|c| c["name"] == "sessions/writable")
        .unwrap();
    assert_eq!(writable["ok"], true);
    assert_eq!(writable["detail"], sessions_root.display().to_string());

    std::fs::remove_dir_all(&dir).ok();
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

    /// The exact JSON in `docs/developing/api.md`'s `## \`search\`` section,
    /// so the doc and the dispatcher cannot drift apart.
    #[test]
    fn search_answers_the_documented_request_with_the_documented_shape() {
        let (paths, root) = temp_paths("search-docs");
        let dir = root.join("2026-09-05-1200");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("raw.jsonl"),
            r#"{"track":"room","start_ms":5000,"end_ms":6000,"text":"the budget review is at noon","confidence":0.9}
"#,
        )
        .unwrap();

        let request: Value = serde_json::from_str(r#"{"query": "budget", "limit": 50}"#).unwrap();
        let got = call("search", &request, &paths).unwrap();
        assert_eq!(
            got,
            json!([{
                "session": "2026-09-05-1200",
                "index": 0,
                "track": "room",
                "start_ms": 5000,
                "speaker": null,
                "text": "the budget review is at noon"
            }])
        );
    }

    /// `limit: 0` must ask for no hits, not one — the truncation check has
    /// to precede the push in `session::search` rather than follow it.
    #[test]
    fn search_with_limit_zero_returns_no_hits() {
        let (paths, root) = temp_paths("search-limit-zero");
        let dir = root.join("2026-09-05-1200");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("raw.jsonl"),
            r#"{"track":"room","start_ms":5000,"end_ms":6000,"text":"the budget review is at noon","confidence":0.9}
"#,
        )
        .unwrap();

        let got = call("search", &json!({"query": "the", "limit": 0}), &paths).unwrap();
        assert_eq!(got, json!([]));
    }

    #[test]
    fn search_query_must_be_a_string() {
        let (paths, _root) = temp_paths("search-query-type");
        let err = call("search", &json!({"query": 5}), &paths).unwrap_err();
        assert!(matches!(err, ApiError::InvalidParams(_)), "{err:?}");
    }

    /// The exact JSON in `docs/developing/api.md`'s `## \`session.update\``
    /// section, so the doc and the dispatcher cannot drift apart.
    #[test]
    fn session_update_answers_the_documented_request_with_the_documented_shape() {
        let (paths, root) = temp_paths("session-update-docs");
        let dir = root.join("2026-09-05-1200");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("session.json"),
            r#"{
                "id": "2026-09-05-1200",
                "name": null,
                "started_at": "2026-09-05T12:00:00+01:00",
                "ended_at": "2026-09-05T12:10:12+01:00",
                "duration_s": 612.5,
                "device_hz": 48000,
                "mic_hz": 48000,
                "channels": 1,
                "mic_channels": 1,
                "apps": [],
                "model": "parakeet",
                "warnings": [],
                "tags": [],
                "notes": "",
                "pinned": false
            }"#,
        )
        .unwrap();

        let request: Value = serde_json::from_str(
            r#"{"session": "2026-09-05-1200", "name": "Standup", "add_tag": "1:1", "pinned": true}"#,
        )
        .unwrap();
        let got = call("session.update", &request, &paths).unwrap();
        assert_eq!(
            got,
            json!({
                "id": "2026-09-05-1200",
                "name": "Standup",
                "started_at": "2026-09-05T12:00:00+01:00",
                "ended_at": "2026-09-05T12:10:12+01:00",
                "duration_s": 612.5,
                "device_hz": 48000,
                "mic_hz": 48000,
                "channels": 1,
                "mic_channels": 1,
                "apps": [],
                "model": "parakeet",
                "warnings": [],
                "tags": ["1:1"],
                "notes": "",
                "pinned": true
            })
        );
    }

    #[test]
    fn session_update_session_must_be_a_string() {
        let (paths, _root) = temp_paths("session-update-type");
        let err = call(
            "session.update",
            &json!({"session": 5, "name": "x"}),
            &paths,
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::InvalidParams(_)), "{err:?}");
    }

    /// The exact JSON in `docs/developing/api.md`'s `## \`session.delete\``
    /// section, so the doc and the dispatcher cannot drift apart.
    #[test]
    fn session_delete_answers_the_documented_request_with_the_documented_shape() {
        let (paths, root) = temp_paths("session-delete-docs");
        let dir = root.join("2026-09-05-1200");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("session.json"), "{}").unwrap();
        std::fs::write(dir.join("transcript.md"), "# transcript\n").unwrap();

        let request: Value = serde_json::from_str(r#"{"session": "2026-09-05-1200"}"#).unwrap();
        let got = call("session.delete", &request, &paths).unwrap();
        assert_eq!(got, json!({"session": "2026-09-05-1200", "deleted": true}));
        assert!(!dir.exists());
    }

    #[test]
    fn session_delete_under_a_transcriber_lock_errors_and_leaves_the_directory() {
        let (paths, root) = temp_paths("session-delete-lock");
        let dir = root.join("2026-09-05-1200");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("session.json"), "{}").unwrap();
        std::fs::write(dir.join("transcript.md"), "# transcript\n").unwrap();
        std::fs::write(
            dir.join(crate::session::TRANSCRIBING_LOCK),
            std::process::id().to_string(),
        )
        .unwrap();

        let err = call(
            "session.delete",
            &json!({"session": "2026-09-05-1200"}),
            &paths,
        )
        .unwrap_err();
        let message = match err {
            ApiError::Failed(m) => m,
            other => panic!("expected Failed, got {other:?}"),
        };
        assert!(message.contains("being transcribed"), "{message}");
        assert!(dir.exists());
    }

    /// The exact JSON in `docs/developing/api.md`'s `## \`export\`` section,
    /// so the doc and the dispatcher cannot drift apart.
    #[test]
    fn export_answers_the_documented_request_with_the_documented_shape() {
        let (paths, root) = temp_paths("export-docs");
        let dir = root.join("2026-09-05-1200");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("raw.jsonl"),
            r#"{"track":"room","start_ms":5000,"end_ms":6000,"text":"the budget review is at noon","confidence":0.9}
"#,
        )
        .unwrap();

        let request: Value =
            serde_json::from_str(r#"{"session": "2026-09-05-1200", "format": "srt"}"#).unwrap();
        let got = call("export", &request, &paths).unwrap();
        assert_eq!(
            got,
            json!({
                "session": "2026-09-05-1200",
                "format": "srt",
                "text": "1\n00:00:05,000 --> 00:00:06,000\nthe budget review is at noon\n"
            })
        );
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

    /// A session fixture with two lines, both `SPEAKER_00`, one per track —
    /// the same shape `session.rs`'s
    /// `undoing_a_naming_puts_the_label_back_on_every_line_it_took` builds, so
    /// `speakers.name` renames exactly 2 lines and not 1 or 3.
    fn speaker_session(root: &Path, id: &str) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let raw = [
            session::RawRecord {
                track: session::Track::Call,
                start_ms: 0,
                end_ms: 2000,
                text: "shall we start with the export spec".into(),
                confidence: 0.9,
            },
            session::RawRecord {
                track: session::Track::Room,
                start_ms: 2100,
                end_ms: 4000,
                text: "yes, go ahead".into(),
                confidence: 0.9,
            },
        ];
        let body: String = raw
            .iter()
            .map(|r| format!("{}\n", serde_json::to_string(r).unwrap()))
            .collect();
        std::fs::write(dir.join("raw.jsonl"), body).unwrap();

        let edits = [
            session::Edit::Speaker {
                target: session::Target {
                    track: session::Track::Call,
                    start_ms: 0,
                },
                name: "SPEAKER_00".into(),
                by: session::DIARIZE_BY.into(),
                at: "2026-01-01T09:00:00+00:00".into(),
            },
            session::Edit::Speaker {
                target: session::Target {
                    track: session::Track::Room,
                    start_ms: 2100,
                },
                name: "SPEAKER_00".into(),
                by: session::DIARIZE_BY.into(),
                at: "2026-01-01T09:00:00+00:00".into(),
            },
        ];
        let body: String = edits
            .iter()
            .map(|e| format!("{}\n", serde_json::to_string(e).unwrap()))
            .collect();
        std::fs::write(dir.join("edits.jsonl"), body).unwrap();
        dir
    }

    /// The same shape as [`speaker_session`], but labelled `call-1`/`room-1`
    /// — diarization's own generated labels, the shape `unnamed_labels`
    /// requires. `SPEAKER_00` does not match `<track>-<n>` and so would leave
    /// `speakers.unnamed` empty.
    fn unnamed_session(root: &Path, id: &str) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let raw = [
            session::RawRecord {
                track: session::Track::Call,
                start_ms: 0,
                end_ms: 2000,
                text: "shall we start with the export spec".into(),
                confidence: 0.9,
            },
            session::RawRecord {
                track: session::Track::Room,
                start_ms: 2100,
                end_ms: 4000,
                text: "yes, go ahead".into(),
                confidence: 0.9,
            },
        ];
        let body: String = raw
            .iter()
            .map(|r| format!("{}\n", serde_json::to_string(r).unwrap()))
            .collect();
        std::fs::write(dir.join("raw.jsonl"), body).unwrap();

        let edits = [
            session::Edit::Speaker {
                target: session::Target {
                    track: session::Track::Call,
                    start_ms: 0,
                },
                name: "call-1".into(),
                by: session::DIARIZE_BY.into(),
                at: "2026-01-01T09:00:00+00:00".into(),
            },
            session::Edit::Speaker {
                target: session::Target {
                    track: session::Track::Room,
                    start_ms: 2100,
                },
                name: "room-1".into(),
                by: session::DIARIZE_BY.into(),
                at: "2026-01-01T09:00:00+00:00".into(),
            },
        ];
        let body: String = edits
            .iter()
            .map(|e| format!("{}\n", serde_json::to_string(e).unwrap()))
            .collect();
        std::fs::write(dir.join("edits.jsonl"), body).unwrap();
        dir
    }

    /// The exact JSON in `docs/developing/api.md`'s `## \`config.get\`` and
    /// `## \`config.set\`` sections. Item 3's first named test: `diarize`
    /// flips off through `config.set` and shows up off through `config.get`.
    #[test]
    fn config_set_answers_the_documented_request_with_the_documented_shape() {
        let (paths, _root) = temp_paths("config-set-docs");

        let request: Value = serde_json::from_str(r#"{"key": "diarize", "value": "off"}"#).unwrap();
        let set = call("config.set", &request, &paths).unwrap();
        assert_eq!(set["diarize"], json!(false));

        let got = call("config.get", &json!({}), &paths).unwrap();
        assert_eq!(got["diarize"], json!(false));
        let keys: std::collections::BTreeSet<&str> = got
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "apps",
                "input_device",
                "diarize",
                "threshold",
                "sessions_dir",
                "devices",
                "default_dir",
                "ask_before_recording",
                "audio_retention",
                "roster",
                "latest_session",
            ]
            .into_iter()
            .collect::<std::collections::BTreeSet<&str>>()
        );
        assert!(got["devices"].is_array());
    }

    /// Item 3's second named test: `config.set` of `sessions_dir` while a
    /// session is recording is `Err(Failed)` mentioning "recording", and
    /// nothing is written.
    #[test]
    fn config_set_of_sessions_dir_refuses_while_a_session_is_recording() {
        let (paths, root) = temp_paths("config-set-live");
        let dir = root.join("2026-09-05-1200").join("audio");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("room.native.wav"), b"live").unwrap();

        let before = Config::load_from(&paths.config_file);
        let err = call(
            "config.set",
            &json!({"key": "sessions_dir", "value": "/tmp/elsewhere"}),
            &paths,
        )
        .unwrap_err();
        match err {
            ApiError::Failed(m) => assert!(m.contains("recording"), "{m}"),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(Config::load_from(&paths.config_file), before);
    }

    #[test]
    fn config_set_of_an_unknown_key_is_invalid_params() {
        let (paths, _root) = temp_paths("config-set-unknown");
        let err = call("config.set", &json!({"key": "nope", "value": "x"}), &paths).unwrap_err();
        assert!(matches!(err, ApiError::InvalidParams(_)), "{err:?}");
    }

    /// The exact JSON in `docs/developing/api.md`'s `## \`roster.add\`` and
    /// `## \`roster.list\`` sections.
    #[test]
    fn roster_add_answers_the_documented_request_with_the_documented_shape() {
        let (paths, _root) = temp_paths("roster-add-docs");

        let request: Value = serde_json::from_str(r#"{"name": "Ana"}"#).unwrap();
        let added = call("roster.add", &request, &paths).unwrap();
        assert_eq!(added, json!(["Ana"]));

        let listed = call("roster.list", &json!({}), &paths).unwrap();
        assert_eq!(listed, json!(["Ana"]));
    }

    /// The exact JSON in `docs/developing/api.md`'s `## \`roster.remove\``
    /// section.
    #[test]
    fn roster_remove_answers_the_documented_request_with_the_documented_shape() {
        let (paths, _root) = temp_paths("roster-remove-docs");
        call("roster.add", &json!({"name": "Ana"}), &paths).unwrap();

        let request: Value = serde_json::from_str(r#"{"name": "Ana"}"#).unwrap();
        let got = call("roster.remove", &request, &paths).unwrap();
        assert_eq!(got, json!([]));
    }

    /// The exact JSON in `docs/developing/api.md`'s `## \`speakers.name\``
    /// and `## \`speakers.undo\`` sections. Item 3's remaining named tests:
    /// `speakers.name` returns `{"renamed": 2}`, and `speakers.undo` run
    /// after it in the same test (so `edits.jsonl` exists) returns
    /// `{"reverted": 2}`.
    #[test]
    fn speakers_name_and_undo_answer_the_documented_request_with_the_documented_shape() {
        let (paths, root) = temp_paths("speakers-name-docs");
        speaker_session(&root, "2026-09-05-1200");

        let request: Value = serde_json::from_str(
            r#"{"session": "2026-09-05-1200", "label": "SPEAKER_00", "name": "Ana"}"#,
        )
        .unwrap();
        let got = call("speakers.name", &request, &paths).unwrap();
        assert_eq!(got, json!({"renamed": 2}));

        let undo_request: Value =
            serde_json::from_str(r#"{"session": "2026-09-05-1200"}"#).unwrap();
        let got = call("speakers.undo", &undo_request, &paths).unwrap();
        assert_eq!(got, json!({"reverted": 2}));
    }

    #[test]
    fn speakers_unnamed_answers_the_documented_request_with_the_documented_shape() {
        let (paths, root) = temp_paths("speakers-unnamed-docs");
        unnamed_session(&root, "2026-09-05-1200");

        let request: Value = serde_json::from_str(r#"{"session": "2026-09-05-1200"}"#).unwrap();
        let got = call("speakers.unnamed", &request, &paths).unwrap();
        assert_eq!(
            got,
            json!([
                {"label": "call-1", "sample": "shall we start with the export spec"},
                {"label": "room-1", "sample": "yes, go ahead"}
            ])
        );
    }

    #[test]
    fn devices_answers_the_documented_request_with_the_documented_shape() {
        let (paths, _root) = temp_paths("devices-docs");
        let got = call("devices", &json!({}), &paths).unwrap();
        assert!(got["devices"].is_array());
    }

    /// Item 3's last named test: the real config and roster files, which
    /// this whole unit is forbidden from touching, are unchanged (or still
    /// absent) after a run that exercises `config.*`, `roster.*` and
    /// `speakers.*` end to end.
    #[test]
    fn the_real_config_and_roster_files_are_never_touched() {
        let before_config = std::fs::read(crate::config::path()).ok();
        let before_roster = std::fs::read(roster::path()).ok();

        let (paths, root) = temp_paths("real-files-untouched");
        speaker_session(&root, "2026-09-05-1200");
        call(
            "config.set",
            &json!({"key": "diarize", "value": "off"}),
            &paths,
        )
        .unwrap();
        call("config.get", &json!({}), &paths).unwrap();
        call("roster.add", &json!({"name": "Ana"}), &paths).unwrap();
        call("roster.list", &json!({}), &paths).unwrap();
        call(
            "speakers.name",
            &json!({"session": "2026-09-05-1200", "label": "SPEAKER_00", "name": "Ana"}),
            &paths,
        )
        .unwrap();
        call(
            "speakers.undo",
            &json!({"session": "2026-09-05-1200"}),
            &paths,
        )
        .unwrap();

        let after_config = std::fs::read(crate::config::path()).ok();
        let after_roster = std::fs::read(roster::path()).ok();
        assert_eq!(before_config, after_config);
        assert_eq!(before_roster, after_roster);
    }
}
