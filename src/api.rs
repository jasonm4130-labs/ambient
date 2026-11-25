//! The one dispatcher for every session method, shared by `ambient mcp` and
//! the window.

use crate::config::Config;
use crate::export;
use crate::roster;
use crate::session;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// `InvalidParams`: the call never ran (JSON-RPC `-32602`). `Failed`: it ran
/// and could not answer (an `isError` tool result).
#[derive(Debug)]
pub enum ApiError {
    InvalidParams(String),
    Failed(String),
}

/// One entry of [`methods`], in the shape MCP `tools/list` reports.
pub struct Method {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

/// The methods advertised as MCP tools. [`call`] answers more, for the CLI
/// and the window only.
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

/// Every file [`call`] touches, so a test can point all of them at temp files.
pub struct Paths {
    pub config_file: PathBuf,
    pub roster_file: PathBuf,
    /// A pinned root. When `None`, `sessions_dir` is re-read from the config
    /// on every call, so a `config.set` of it moves the very next call.
    pub sessions_root: Option<PathBuf>,
}

impl Paths {
    /// Never calls [`session::home`], which reaches the user's real files.
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

/// New methods are new match arms, never new arguments.
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

fn doctor_checks(paths: &Paths) -> Value {
    json!(crate::doctor::run(
        session::models_root(),
        crate::engine::locate(),
        &paths.config_file,
        &paths.root(),
    ))
}

/// Enumerated by [`session_dirs`], not [`session::summaries`], which follows
/// symlinks. A session with a symlinked file is listed with `error` set and
/// nothing behind the link read.
fn sessions(root: &Path) -> Value {
    let mut listed: Vec<session::SessionSummary> = session_dirs(root)
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
                notes: String::new(),
                warnings: Vec::new(),
                audio_available: false,
                pinned: false,
            }),
            None => session::summarise(dir),
        })
        .collect();
    listed.sort_by_key(|s| !s.pinned);
    json!(listed)
}

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

/// The cursor counts lines in `raw.jsonl` order, not by `start_ms`: room and
/// call tracks have independent clocks, so a time cursor would skip call
/// lines written later.
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

/// The lock is claimed here, not in [`session::update_meta`], so the
/// transcriber can call that while holding its own lock.
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

/// Does not ask: the window confirms before calling.
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

/// No lock: the transcriber never needs to exclude a reader.
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

/// Real CoreAudio devices, so tests assert only the shape.
fn input_device_names() -> Vec<String> {
    crate::capture::input_devices()
        .into_iter()
        .map(|(_, name)| name)
        .collect()
}

/// Reads only through `paths`: `Config::load`, `roster::load` and
/// `session::home` reach the user's real files.
fn config_get(paths: &Paths) -> Value {
    let cfg = Config::load_from(&paths.config_file);
    let home = std::env::var("HOME").unwrap_or_default();
    let latest = session::latest(&paths.root());
    json!({
        "apps": cfg.apps,
        "input_device": cfg.input_device,
        "diarize": cfg.diarize,
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

fn roster_list(paths: &Paths) -> Value {
    json!(roster::load_from(&paths.roster_file))
}

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

/// An id must be one path segment, checked before anything is opened, and a
/// link in the folder is not a session.
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

fn state(dir: &Path) -> &'static str {
    if session::is_growing(&dir.join("audio").join("room.native.wav")) {
        "live"
    } else if session::live_transcriber(dir).is_some() {
        "transcribing"
    } else if dir.join("transcript.md").is_file() {
        "done"
    } else if std::fs::read_to_string(dir.join(session::STATUS_FILE))
        .is_ok_and(|s| s.starts_with("failed"))
    {
        "failed"
    } else {
        "pending"
    }
}

const READ_FROM_SESSIONS: [&str; 3] = ["session.json", "raw.jsonl", "edits.jsonl"];

/// A directory inside the root says nothing about where a file in it points.
fn symlinked(dir: &Path) -> Option<String> {
    READ_FROM_SESSIONS
        .iter()
        .map(|name| dir.join(name))
        .find(|p| std::fs::symlink_metadata(p).is_ok_and(|m| !m.is_file()))
        .map(|p| format!("refusing symlink {}", p.display()))
}

/// Every field comes from [`session_dirs`]: `session::live_session_in` and
/// `session::captured_awaiting_transcript` follow symlinks. `live` repeats
/// `live_session_in`'s rule rather than filtering its answer, which a link
/// sorting after the real live session would hide.
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

/// Unlike [`session::list`], a link in the sessions folder is never a session.
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

/// Outside `mod tests` so `cargo test api::doctor` matches it.
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
    assert_eq!(checks.len(), 9);

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
    let expected = crate::doctor::run(
        session::models_root(),
        crate::engine::locate(),
        &paths.config_file,
        &paths.root(),
    );
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

    #[test]
    fn sessions_returns_saved_notes_and_pins_before_newer_sessions() {
        let (paths, root) = temp_paths("notes-and-pins");
        for (id, pinned) in [("older", true), ("recent", false)] {
            let dir = root.join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("session.json"),
                json!({
                    "id": id, "name": null, "started_at": "2026-09-06", "ended_at": "",
                    "duration_s": 1, "device_hz": 16000, "channels": 1,
                    "mic_channels": 1, "apps": [], "model": "test",
                    "notes": "Keep the budget decision", "pinned": pinned
                })
                .to_string(),
            )
            .unwrap();
        }
        let listed = call("sessions", &json!({}), &paths).unwrap();
        assert_eq!(listed[0]["id"], "older");
        assert_eq!(listed[0]["notes"], "Keep the budget decision");
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

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

    /// Pinned to the example in `docs/developing/api.md`.
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

    /// Pinned to the example in `docs/developing/api.md`.
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
                "pinned": true,
                "capture": "all"
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

    /// Pinned to the example in `docs/developing/api.md`.
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

    /// Pinned to the example in `docs/developing/api.md`.
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

    #[test]
    fn paths_carries_a_roster_file_untouched_here() {
        let (paths, _root) = temp_paths("roster-field");
        assert!(!paths.roster_file.exists());
        assert!(roster::path() != paths.roster_file);
    }

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

    /// `unnamed_labels` only reports generated `<track>-<n>` labels, never
    /// `SPEAKER_00`.
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

    /// Pinned to the example in `docs/developing/api.md`.
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

    /// Pinned to the example in `docs/developing/api.md`.
    #[test]
    fn roster_add_answers_the_documented_request_with_the_documented_shape() {
        let (paths, _root) = temp_paths("roster-add-docs");

        let request: Value = serde_json::from_str(r#"{"name": "Ana"}"#).unwrap();
        let added = call("roster.add", &request, &paths).unwrap();
        assert_eq!(added, json!(["Ana"]));

        let listed = call("roster.list", &json!({}), &paths).unwrap();
        assert_eq!(listed, json!(["Ana"]));
    }

    /// Pinned to the example in `docs/developing/api.md`.
    #[test]
    fn roster_remove_answers_the_documented_request_with_the_documented_shape() {
        let (paths, _root) = temp_paths("roster-remove-docs");
        call("roster.add", &json!({"name": "Ana"}), &paths).unwrap();

        let request: Value = serde_json::from_str(r#"{"name": "Ana"}"#).unwrap();
        let got = call("roster.remove", &request, &paths).unwrap();
        assert_eq!(got, json!([]));
    }

    /// Pinned to the example in `docs/developing/api.md`.
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
