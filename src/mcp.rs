//! `ambient mcp` — the sessions on disk, read over MCP.
//!
//! A synchronous JSON-RPC 2.0 loop over newline-delimited stdio: one message
//! per line in, one response line out, nothing else on stdout. There is no
//! async runtime and no MCP SDK, because the protocol this server needs is a
//! `match` on a string and the SDK would be the largest dependency in the
//! tree.
//!
//! [`serve`] takes its reader, writer and sessions root as arguments so the
//! tests drive the whole loop with in-memory buffers and a temp directory —
//! the same code path a client gets, with no process to spawn.

use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use crate::session;

/// The newest revision this server implements, and the one an unrecognised
/// request is answered with. `2026-07-28` retired the `initialize` handshake
/// this loop is built on, so it is deliberately not claimed.
const LATEST: &str = "2025-11-25";

/// Every revision whose `initialize` this server will echo back. Anything else
/// gets [`LATEST`], which is what the lifecycle spec asks of a server that
/// cannot speak what was asked for.
const SUPPORTED: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];

/// Read JSON-RPC messages until EOF, writing one response line each.
///
/// Nothing but responses is ever written to `writer`: diagnostics belong on
/// stderr, because a stray line here corrupts the stream for the client. The
/// loop is deliberately unkillable by input — every failure inside becomes a
/// response, so a malformed line costs the client an error and nothing else.
pub fn serve<R: BufRead, W: Write>(reader: R, mut writer: W, root: &Path) -> Result<()> {
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = handle(&line, root) {
            writeln!(writer, "{response}")?;
            writer.flush()?;
        }
    }
    Ok(())
}

/// One request line to its response, or `None` for a notification — a message
/// with no `id`, which the spec forbids replying to.
pub fn handle(line: &str, root: &Path) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
    };
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str);
    let (Some(method), Some("2.0")) = (method, msg.get("jsonrpc").and_then(Value::as_str)) else {
        return Some(error(
            id.unwrap_or(Value::Null),
            -32600,
            "invalid request: expected \"jsonrpc\":\"2.0\" and a string \"method\"",
        ));
    };
    let id = id?;
    Some(match method {
        "initialize" => response(id, initialize(&msg)),
        "ping" => response(id, json!({})),
        "tools/list" => response(id, json!({ "tools": tools() })),
        "tools/call" => match call(&msg, root) {
            Ok(result) => response(id, result),
            Err(e) => error(id, -32602, &e),
        },
        other => error(id, -32601, &format!("no such method {other:?}")),
    })
}

fn response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// The client's version if this server speaks it, else the newest one it does.
fn initialize(msg: &Value) -> Value {
    let asked = msg["params"]["protocolVersion"]
        .as_str()
        .unwrap_or_default();
    let version = if SUPPORTED.contains(&asked) {
        asked
    } else {
        LATEST
    };
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "ambient", "version": env!("CARGO_PKG_VERSION")},
    })
}

fn tools() -> Value {
    json!([
        {
            "name": "sessions",
            "description": "Every session recorded on this machine, newest first: id, name, when it started, how long it ran, and what is happening to it now.",
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "transcript",
            "description": "One session's transcript, including the session being recorded right now.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": {"type": "string", "description": "The session id, as `sessions` reports it."},
                    "since": {"type": "integer", "description": "Skip this many lines from the start, so a caller polling a live session reads only what is new."},
                    "verbatim": {"type": "boolean", "description": "Return what the recogniser heard, before repairs and speaker names."},
                },
                "required": ["session"],
            },
        },
        {
            "name": "status",
            "description": "Whether a recording is in progress, and how many sessions are on disk or waiting to be transcribed.",
            "inputSchema": {"type": "object", "properties": {}},
        },
    ])
}

/// Route a `tools/call`. `Err` is the message for a `-32602`: the request was
/// unroutable. A tool that ran and failed returns `Ok` of an `isError` result
/// instead, because that is a fact about the sessions, not about the protocol.
fn call(msg: &Value, root: &Path) -> Result<Value, String> {
    let params = msg
        .get("params")
        .and_then(Value::as_object)
        .ok_or("tools/call needs an object \"params\"")?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("tools/call needs a string \"name\"")?;
    // Checked here and unused until the tools land: a client sending the wrong
    // shape should hear about it from the first release, not the second.
    let _arguments = params
        .get("arguments")
        .and_then(Value::as_object)
        .ok_or("tools/call needs an object \"arguments\"")?;
    match name {
        "status" => Ok(ok_result(&status(root))),
        "sessions" | "transcript" => Ok(tool_error("not implemented")),
        other => Err(format!("no such tool {other:?}")),
    }
}

/// A tool's JSON value as MCP carries it: one text block holding the compact
/// serialisation, which is what a client that cannot parse structured content
/// still gets to read.
fn ok_result(value: &Value) -> Value {
    json!({"content": [{"type": "text", "text": value.to_string()}], "isError": false})
}

fn tool_error(message: &str) -> Value {
    json!({"content": [{"type": "text", "text": message}], "isError": true})
}

fn status(root: &Path) -> Value {
    let live = session::live_session_in(root)
        .and_then(|d| d.file_name().map(|n| json!({"id": n.to_string_lossy()})))
        .unwrap_or(Value::Null);
    json!({
        "live": live,
        "awaiting_transcript": session::captured_awaiting_transcript(root).len(),
        "sessions": session_dirs(root).len(),
    })
}

/// Every session directory under `root`, oldest first.
///
/// `session::list` follows symlinks, so a link planted in the sessions folder
/// would let a caller read a directory outside it. This walk keeps entries
/// whose own `symlink_metadata` says directory, then keeps the ones that hold
/// a `session.json` or a growing native scratch wav — the two ends of a
/// recording's life, which is what keeps a stray `notes` folder out. Both the
/// `sessions` tool and `status.sessions` count this, so they always agree.
fn session_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut all: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()))
        .filter(|p| {
            p.join("session.json").is_file()
                || session::is_growing(&p.join("audio").join("room.native.wav"))
        })
        .collect();
    all.sort();
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};

    /// Feed `lines` to [`serve`] and parse whatever came back. One call per
    /// test keeps the loop, not a handler, as the thing under test.
    fn run(root: &Path, lines: &[&str]) -> Vec<Value> {
        let input = format!("{}\n", lines.join("\n"));
        let mut out: Vec<u8> = Vec::new();
        serve(input.as_bytes(), &mut out, root).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn root(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ambient-mcp-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&p).ok();
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// The one text block every tool result carries, parsed back to JSON.
    fn text(result: &Value) -> Value {
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn initialize_names_this_server_and_its_one_capability() {
        let out = run(
            &root("init"),
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
            ],
        );
        assert_eq!(out.len(), 1);
        let res = &out[0]["result"];
        assert_eq!(res["protocolVersion"], "2025-11-25");
        assert!(res["capabilities"]["tools"].is_object());
        assert_eq!(res["serverInfo"]["name"], "ambient");
        assert_eq!(res["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    }

    /// A version this server does not speak gets the latest one it does, which
    /// is what the lifecycle spec asks for — the client then decides whether to
    /// carry on.
    #[test]
    fn an_unsupported_protocol_version_is_answered_with_the_latest_supported_one() {
        let out = run(
            &root("version"),
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
            ],
        );
        assert_eq!(out[0]["result"]["protocolVersion"], "2025-11-25");
    }

    #[test]
    fn a_notification_gets_no_reply() {
        let out = run(
            &root("notify"),
            &[r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#],
        );
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn ping_echoes_the_id_it_was_given() {
        let out = run(
            &root("ping"),
            &[r#"{"jsonrpc":"2.0","id":"a","method":"ping"}"#],
        );
        assert_eq!(out[0]["id"], "a");
        assert_eq!(out[0]["result"], json!({}));
    }

    #[test]
    fn tools_list_advertises_the_three_reading_tools() {
        let out = run(
            &root("list"),
            &[r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#],
        );
        let tools = out[0]["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["sessions", "transcript", "status"]);
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{t:?}");
        }
    }

    /// `status` counts what `sessions` would list, so the two never disagree:
    /// a directory with no session in it does not count, and a symlink out of
    /// the root is not followed.
    #[test]
    fn status_counts_the_sessions_and_names_the_live_one() {
        let r = root("status");
        let done = r.join("2026-01-01T0900");
        std::fs::create_dir_all(&done).unwrap();
        std::fs::write(done.join("session.json"), r#"{"id":"2026-01-01T0900"}"#).unwrap();
        std::fs::write(done.join("transcript.md"), "# transcript\n").unwrap();
        std::fs::create_dir_all(r.join("notes")).unwrap();
        let away = std::env::temp_dir().join(format!("ambient-mcp-away-{}", std::process::id()));
        std::fs::remove_dir_all(&away).ok();
        std::fs::create_dir_all(&away).unwrap();
        std::fs::write(away.join("session.json"), r#"{"id":"away"}"#).unwrap();
        std::os::unix::fs::symlink(&away, r.join("outside")).unwrap();
        let live = r.join("live").join("audio");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("room.native.wav"), b"RIFF").unwrap();

        let out = run(
            &r,
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"status","arguments":{}}}"#,
            ],
        );
        assert_eq!(out[0]["result"]["isError"], false);
        assert_eq!(
            text(&out[0]["result"]),
            json!({"live": {"id": "live"}, "awaiting_transcript": 0, "sessions": 2})
        );
    }

    #[test]
    fn tools_call_rejects_a_request_it_cannot_route() {
        let out = run(
            &root("badcall"),
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call"}"#,
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":5,"arguments":{}}}"#,
            ],
        );
        assert_eq!(out.len(), 3);
        for r in &out {
            assert_eq!(r["error"]["code"], -32602, "{r:?}");
        }
    }

    /// Until the tools land, calling one says so rather than pretending: a
    /// tool that ran and failed is a result with `isError`, not a protocol
    /// error.
    #[test]
    fn the_unimplemented_tools_report_a_tool_error() {
        let out = run(
            &root("todo"),
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"sessions","arguments":{}}}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"transcript","arguments":{"session":"a"}}}"#,
            ],
        );
        for r in &out {
            assert_eq!(r["result"]["isError"], true, "{r:?}");
            assert_eq!(
                r["result"]["content"][0]["text"], "not implemented",
                "{r:?}"
            );
        }
    }

    /// Every error class in one pass, ending in a `ping`: a loop that answered
    /// the last line survived all of them, which is the property that matters
    /// more than any single code.
    #[test]
    fn every_malformed_line_is_answered_and_the_loop_survives() {
        let out = run(
            &root("errors"),
            &[
                "not json at all",
                r#"{"id":1,"method":"ping"}"#,
                r#"{"jsonrpc":"2.0","id":1,"method":7}"#,
                r#"{"jsonrpc":"2.0","id":1,"method":"no/such/method"}"#,
                "",
                r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#,
            ],
        );
        assert_eq!(out.len(), 5, "{out:?}");
        assert_eq!(out[0]["id"], Value::Null);
        assert_eq!(out[0]["error"]["code"], -32700);
        assert_eq!(out[1]["error"]["code"], -32600);
        assert_eq!(out[2]["error"]["code"], -32600);
        assert_eq!(out[3]["error"]["code"], -32601);
        assert_eq!(out[4]["id"], 9);
        assert_eq!(out[4]["result"], json!({}));
    }
}
