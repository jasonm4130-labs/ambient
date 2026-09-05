//! `ambient mcp` — the sessions on this machine, read over MCP.
//!
//! A synchronous JSON-RPC 2.0 loop over newline-delimited stdio: one message
//! per line in, one per line out, nothing else on stdout ever. No async
//! runtime and no MCP SDK, because the protocol that matters here is four
//! methods wide and an SDK would be more code than the server.
//!
//! The loop never dies on input. Every way a line can be wrong — down to
//! bytes that are not UTF-8 — becomes an error *response*, so a client that
//! sends one bad line keeps its session. [`serve`] takes its reader, writer
//! and sessions folder as arguments so the tests drive exactly what `main`
//! drives, with buffers in place of pipes.

use crate::api::{self, ApiError, Paths};
use crate::config;
use crate::roster;
#[cfg(test)]
use crate::session;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

/// The revision this server implements, and the only one it claims. A client
/// offering any other gets this back rather than its own version, which is
/// what the lifecycle spec asks of a server that does not speak what it was
/// offered. Claiming a revision means implementing all of it, and one is what
/// this loop has been written and tested against.
const PROTOCOL: &str = "2025-11-25";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Read requests from `reader` until EOF, writing one response line each to
/// `writer`. Returns `Ok(())` at EOF; only a broken pipe or unreadable stdin
/// is an error, since everything else is answered on the wire.
pub fn serve<R: BufRead, W: Write>(mut reader: R, mut writer: W, root: &Path) -> Result<()> {
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        // Bytes rather than `lines()`, which turns a line that is not UTF-8
        // into an `Err` and so takes the whole stream down with it. Lossy
        // decoding leaves such a line as a parse error the client is told
        // about, and the requests after it are still answered.
        if reader.read_until(b'\n', &mut buffer)? == 0 {
            return Ok(());
        }
        let line = String::from_utf8_lossy(&buffer);
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = handle(&line, root) {
            writeln!(writer, "{response}")?;
            // Flushed per message: the client is blocked on this line, and a
            // buffered answer to a request nobody follows up is a hang.
            writer.flush()?;
        }
    }
}

/// One request line to its response, or `None` for a notification — which has
/// no `id` and which the spec forbids answering, even to complain.
pub fn handle(line: &str, root: &Path) -> Option<Value> {
    let request: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(failure(Value::Null, PARSE_ERROR, &e.to_string())),
    };
    let Some(object) = request.as_object() else {
        return Some(failure(Value::Null, INVALID_REQUEST, "expected an object"));
    };
    let id = object.get("id").cloned()?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(failure(id, INVALID_REQUEST, r#"expected "jsonrpc":"2.0""#));
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Some(failure(id, INVALID_REQUEST, "method must be a string"));
    };
    let params = object.get("params");
    let answered = match method {
        "initialize" => Ok(initialize()),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call(params, root),
        other => {
            let message = format!("no such method {other:?}");
            return Some(failure(id, METHOD_NOT_FOUND, &message));
        }
    };
    Some(match answered {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(e) => failure(id, e.code, &e.message),
    })
}

/// A protocol-level refusal: the request was malformed, so no tool ran. A tool
/// that ran and failed is a *result* with `isError` instead, which is what
/// lets a model read the reason rather than see a transport error.
struct Refusal {
    code: i64,
    message: String,
}

fn invalid_params(message: impl Into<String>) -> Refusal {
    Refusal {
        code: INVALID_PARAMS,
        message: message.into(),
    }
}

fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn initialize() -> Value {
    json!({
        "protocolVersion": PROTOCOL,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": "ambient", "version": env!("CARGO_PKG_VERSION")},
    })
}

/// The three tools `tools/list` reports, selected out of [`api::methods`] by
/// name rather than rendered wholesale: `tools_list_describes_the_three_tools`
/// asserts this list by whole-array equality, so a later unit's new
/// `api::methods()` entry must not silently appear here. `cargo test mcp::`
/// staying green after later units add methods is the proof this held.
const MCP_TOOLS: [&str; 3] = ["sessions", "transcript", "status"];

fn tools() -> Value {
    let all = api::methods();
    let ordered: Vec<Value> = MCP_TOOLS
        .iter()
        .map(|name| {
            let m = all.iter().find(|m| m.name == *name).unwrap_or_else(|| {
                panic!("MCP_TOOLS names a method api::methods() does not have: {name}")
            });
            json!({
                "name": m.name,
                "description": m.description,
                "inputSchema": m.input_schema,
            })
        })
        .collect();
    Value::Array(ordered)
}

/// `tools/call`. Arguments are validated before anything on disk is touched,
/// then routed through [`api::call`] — the one dispatcher `ambient mcp` and
/// (later) the window both call.
fn call(params: Option<&Value>, root: &Path) -> Result<Value, Refusal> {
    let params = params
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_params("tools/call needs an object `params`"))?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_params("tools/call needs a string `name`"))?;
    // Absent `arguments` reads as `{}` — the two tools that take none are
    // called without it by real clients — but a non-object is a client bug.
    if params.get("arguments").is_some_and(|a| !a.is_object()) {
        return Err(invalid_params("`arguments` must be an object"));
    }
    let empty = json!({});
    let arguments = params.get("arguments").unwrap_or(&empty);
    let paths = Paths {
        config_file: config::path(),
        roster_file: roster::path(),
        sessions_root: Some(root.to_path_buf()),
    };
    match api::call(name, arguments, &paths) {
        Ok(v) => Ok(content(&v, false)),
        Err(ApiError::InvalidParams(m)) => Err(invalid_params(m)),
        Err(ApiError::Failed(m)) => Ok(content(&Value::String(m), true)),
    }
}

/// A tool result. The value is serialised compactly into one text block, which
/// is how a JSON answer reaches a model through MCP's content array.
fn content(value: &Value, is_error: bool) -> Value {
    let text = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Feed request lines through `serve` and parse what came back, one value
    /// per output line. Every test below is this one exchange: they drive the
    /// same entry point `main` does, so nothing about the loop is mocked.
    fn exchange(root: &Path, requests: &[&str]) -> Vec<Value> {
        let input = requests.join("\n") + "\n";
        let mut out: Vec<u8> = Vec::new();
        serve(input.as_bytes(), &mut out, root).expect("serve reached EOF");
        String::from_utf8(out)
            .expect("utf-8 output")
            .lines()
            .map(|l| serde_json::from_str(l).expect("each line is one JSON message"))
            .collect()
    }

    /// A scratch sessions folder of its own per test, since these run in one
    /// process and a shared name would have them treading on each other.
    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ambient-mcp-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn initialize_names_the_protocol_and_this_server() {
        let root = scratch("init");
        let got = exchange(
            &root,
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
            ],
        );
        let result = &got[0]["result"];
        assert_eq!(result["protocolVersion"], "2025-11-25");
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "ambient");
        assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    }

    /// The lifecycle spec's answer to a revision we do not speak: name the
    /// latest we do and let the client decide, rather than fail the handshake.
    #[test]
    fn an_unsupported_protocol_version_gets_the_latest_we_speak() {
        let root = scratch("proto");
        let got = exchange(
            &root,
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
            ],
        );
        assert_eq!(got[0]["result"]["protocolVersion"], "2025-11-25");
    }

    /// A notification has no `id`, and answering one is a protocol violation.
    #[test]
    fn notifications_are_not_answered() {
        let root = scratch("notify");
        let got = exchange(
            &root,
            &[r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#],
        );
        assert!(got.is_empty(), "expected no output, got {got:?}");
    }

    #[test]
    fn ping_returns_an_empty_result_under_the_id_it_was_asked_with() {
        let root = scratch("ping");
        let got = exchange(&root, &[r#"{"jsonrpc":"2.0","id":"a","method":"ping"}"#]);
        assert_eq!(got[0]["id"], "a");
        assert_eq!(got[0]["result"], json!({}));
    }

    #[test]
    fn tools_list_describes_the_three_tools() {
        let root = scratch("list");
        let got = exchange(
            &root,
            &[r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#],
        );
        let tools = got[0]["result"]["tools"].as_array().unwrap().clone();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["sessions", "transcript", "status"]);
        for t in &tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        }
        let schema = &tools[1]["inputSchema"];
        assert_eq!(schema["properties"]["session"]["type"], "string");
        assert_eq!(schema["properties"]["since"]["type"], "integer");
        assert_eq!(schema["properties"]["verbatim"]["type"], "boolean");
        assert_eq!(schema["required"], json!(["session"]));
    }

    /// `status` counts the directories that are sessions, and only those: a
    /// stray folder is not one, and no field is answered from behind a symlink
    /// out of the root.
    ///
    /// The two links here are shaped to be caught by each of the fields that
    /// does not do its own walking: `outside` is what
    /// `session::captured_awaiting_transcript` counts, `outside-live` is what
    /// `session::live_session_in` would name — and it sorts after the real
    /// live session, so a filter applied to that function's answer rather than
    /// to the set it picks from would report no live session at all.
    #[test]
    fn status_counts_the_session_directories_and_names_the_live_one() {
        let root = scratch("status");
        let done = root.join("2026-09-05-1200");
        std::fs::create_dir_all(&done).unwrap();
        std::fs::write(done.join("session.json"), "{}").unwrap();
        std::fs::write(done.join("transcript.md"), "# transcript\n").unwrap();
        std::fs::create_dir_all(root.join("notes")).unwrap();
        let outside = scratch("status-outside");
        std::fs::create_dir_all(outside.join("audio")).unwrap();
        std::fs::write(outside.join("session.json"), "{}").unwrap();
        std::fs::write(outside.join("audio").join("room.wav"), b"RIFF....").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("outside")).unwrap();
        let outside_live = scratch("status-outside-live");
        std::fs::create_dir_all(outside_live.join("audio")).unwrap();
        std::fs::write(
            outside_live.join("audio").join("room.native.wav"),
            b"RIFF....",
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside_live, root.join("outside-live")).unwrap();
        let live = root.join("live");
        std::fs::create_dir_all(live.join("audio")).unwrap();
        std::fs::write(live.join("audio").join("room.native.wav"), b"RIFF....").unwrap();

        let got = exchange(
            &root,
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"status","arguments":{}}}"#,
            ],
        );
        let result = &got[0]["result"];
        assert_eq!(result["isError"], false);
        let text = result["content"][0]["text"].as_str().unwrap();
        let status: Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            status,
            json!({"live": {"id": "live"}, "awaiting_transcript": 0, "sessions": 2})
        );
    }

    /// A call the server cannot even attempt is a protocol error, not a tool
    /// that ran and failed — the request itself was malformed.
    #[test]
    fn a_malformed_tool_call_is_invalid_params() {
        let root = scratch("call-args");
        let got = exchange(
            &root,
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call"}"#,
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":5}}"#,
            ],
        );
        for r in &got {
            assert_eq!(r["error"]["code"], -32602, "{r}");
        }
    }

    /// Every way a line can be wrong, and then a `ping`: the loop answers what
    /// it can and reads on, because a client that sent one bad line still has
    /// a session to finish.
    #[test]
    fn bad_lines_are_reported_and_the_loop_reads_on() {
        let root = scratch("errors");
        let got = exchange(
            &root,
            &[
                "not json at all",
                r#"{"id":1,"method":"ping"}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":7}"#,
                r#"{"jsonrpc":"2.0","id":3,"method":"no/such/method"}"#,
                r#"{"jsonrpc":"2.0","id":4,"method":"ping"}"#,
            ],
        );
        assert_eq!(got[0]["id"], Value::Null);
        assert_eq!(got[0]["error"]["code"], -32700);
        assert_eq!(got[1]["error"]["code"], -32600);
        assert_eq!(got[2]["error"]["code"], -32600);
        assert_eq!(got[3]["error"]["code"], -32601);
        assert_eq!(got[4]["result"], json!({}));
    }

    /// A line of bytes that is not UTF-8 is a bad request like any other. It
    /// arrives from a pipe, not from a well-behaved client, and taking the
    /// rest of the stream down with it would drop requests that were fine.
    #[test]
    fn a_line_that_is_not_utf8_does_not_end_the_session() {
        let root = scratch("bytes");
        let mut out: Vec<u8> = Vec::new();
        serve(
            &b"\xff\xfe garbage\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n"[..],
            &mut out,
            &root,
        )
        .expect("serve reached EOF");
        let got: Vec<Value> = String::from_utf8(out)
            .expect("utf-8 output")
            .lines()
            .map(|l| serde_json::from_str(l).expect("a JSON line"))
            .collect();
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0]["error"]["code"], -32700);
        assert_eq!(got[1]["id"], 1);
        assert_eq!(got[1]["result"], json!({}));
    }

    /// Metadata as a closed capture writes it.
    fn meta(id: &str) -> String {
        format!(
            r#"{{"id":"{id}","name":"stand-up","started_at":"2026-09-05T12:00:00+01:00",
                 "ended_at":"2026-09-05T12:00:12+01:00","duration_s":12.5,"device_hz":48000,
                 "mic_hz":48000,"channels":1,"mic_channels":1,"apps":[],"model":"parakeet"}}"#
        )
    }

    fn raw_line(track: &str, start_ms: u64) -> String {
        let end_ms = start_ms + 1_000;
        format!(
            r#"{{"track":"{track}","start_ms":{start_ms},"end_ms":{end_ms},"text":"{track} at {start_ms}","confidence":0.9}}"#
        )
    }

    /// A finished session: metadata, the raw lines in the order they were
    /// appended, and a transcript.
    fn transcribed(root: &Path, id: &str, lines: &[(&str, u64)]) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("session.json"), meta(id)).unwrap();
        let body: String = lines.iter().map(|(t, s)| raw_line(t, *s) + "\n").collect();
        std::fs::write(dir.join("raw.jsonl"), body).unwrap();
        std::fs::write(dir.join("transcript.md"), "# transcript\n").unwrap();
        dir
    }

    fn call_tool(root: &Path, name: &str, arguments: &str) -> Value {
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{name}","arguments":{arguments}}}}}"#
        );
        exchange(root, &[&request]).remove(0)
    }

    /// What a tool answered, parsed back out of its text block, and whether it
    /// reported failure. A failed tool's text is a message, not JSON, so it
    /// comes back as a string.
    fn payload(response: &Value) -> (Value, bool) {
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("a text block, got {response}"));
        let value = serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()));
        (value, response["result"]["isError"].as_bool().unwrap())
    }

    /// The sessions folder is the boundary. A link in it is not a session
    /// whatever it points at, and a session whose `raw.jsonl` points out of
    /// the folder is listed as broken rather than followed.
    #[test]
    fn sessions_lists_the_root_and_never_what_a_link_points_at() {
        let root = scratch("sessions");
        transcribed(&root, "2026-09-05-1200", &[("room", 0), ("room", 5_000)]);
        std::os::unix::fs::symlink(root.join("2026-09-05-1200"), root.join("link")).unwrap();
        let outside = scratch("sessions-outside");
        std::fs::write(outside.join("session.json"), meta("outside")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("outside")).unwrap();
        let leak = root.join("leak");
        std::fs::create_dir_all(&leak).unwrap();
        std::fs::write(leak.join("session.json"), meta("leak")).unwrap();
        std::os::unix::fs::symlink(outside.join("session.json"), leak.join("raw.jsonl")).unwrap();

        let (got, is_error) = payload(&call_tool(&root, "sessions", "{}"));
        assert!(!is_error, "{got}");
        let listed = got.as_array().unwrap();
        let ids: Vec<&str> = listed.iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["leak", "2026-09-05-1200"], "newest first, no links");
        let by = |id: &str| listed.iter().find(|s| s["id"] == id).unwrap().clone();
        assert_eq!(by("2026-09-05-1200")["transcribed"], true);
        assert!(
            by("leak")["error"]
                .as_str()
                .is_some_and(|e| e.contains("refusing symlink")),
            "{got}"
        );
        assert!(
            by("leak")["duration_s"].is_null(),
            "no metadata for a session we would not read"
        );
    }

    /// `since` counts lines already seen and `next` is what to send back, so a
    /// caller polling a session in flight sees each line exactly once — and a
    /// cursor past the end is caught up rather than an error.
    #[test]
    fn transcript_returns_the_lines_after_the_cursor() {
        let root = scratch("cursor");
        transcribed(&root, "2026-09-05-1200", &[("room", 0), ("room", 5_000)]);
        let ask = |extra: &str| {
            let args = format!(r#"{{"session":"2026-09-05-1200"{extra}}}"#);
            payload(&call_tool(&root, "transcript", &args))
        };

        let (all, is_error) = ask("");
        assert!(!is_error, "{all}");
        assert_eq!(all["session"], "2026-09-05-1200");
        assert_eq!(all["state"], "done");
        assert_eq!(all["next"], 2);
        assert_eq!(all["lines"].as_array().unwrap().len(), 2);

        let (one, _) = ask(r#","since":1"#);
        assert_eq!(one["lines"].as_array().unwrap().len(), 1);
        assert_eq!(one["lines"][0]["start_ms"], 5_000);
        assert_eq!(one["next"], 2);

        for since in [r#","since":2"#, r#","since":9"#] {
            let (caught_up, _) = ask(since);
            assert_eq!(caught_up["lines"], json!([]), "{caught_up}");
            assert_eq!(caught_up["next"], 2, "the length, whatever was asked for");
        }
    }

    /// `raw.jsonl` is written room track first and then call track, and the
    /// two clocks are independent — so the cursor counts appended lines. A
    /// cursor on `start_ms` would skip the call line at 0 for ever.
    #[test]
    fn the_cursor_counts_appended_lines_and_not_the_clock() {
        let root = scratch("race");
        transcribed(&root, "2026-09-05-1200", &[("room", 5_000), ("call", 0)]);
        let (got, _) = payload(&call_tool(
            &root,
            "transcript",
            r#"{"session":"2026-09-05-1200","since":1}"#,
        ));
        let lines = got["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 1, "{got}");
        assert_eq!(lines[0]["track"], "call");
        assert_eq!(lines[0]["start_ms"], 0);
    }

    /// An argument of the wrong JSON type is the client's bug, not a session
    /// that could not be read: it is refused before the disk is touched.
    #[test]
    fn arguments_of_the_wrong_type_are_invalid_params() {
        let root = scratch("types");
        transcribed(&root, "2026-09-05-1200", &[("room", 0)]);
        for args in [
            r#"{"session":"2026-09-05-1200","since":"1"}"#,
            r#"{"session":"2026-09-05-1200","verbatim":"yes"}"#,
            r#"{"session":5}"#,
            "{}",
        ] {
            let got = call_tool(&root, "transcript", args);
            assert_eq!(got["error"]["code"], -32602, "{args} -> {got}");
        }
    }

    /// An id that is not one path segment inside the root never reaches the
    /// filesystem, and one that names nothing says so in words a model can
    /// act on.
    #[test]
    fn an_id_that_is_not_a_session_in_the_root_is_a_tool_error() {
        let root = scratch("ids");
        transcribed(&root, "2026-09-05-1200", &[("room", 0)]);
        std::os::unix::fs::symlink(root.join("2026-09-05-1200"), root.join("link")).unwrap();
        for id in ["../x", "", ".", "link"] {
            let args = format!(r#"{{"session":"{id}"}}"#);
            let (got, is_error) = payload(&call_tool(&root, "transcript", &args));
            assert!(is_error, "{id} -> {got}");
        }
        let (got, is_error) = payload(&call_tool(&root, "transcript", r#"{"session":"nope"}"#));
        assert!(is_error, "{got}");
        assert!(got.as_str().unwrap().contains("no session nope"), "{got}");
    }

    /// The session directory is real, but the file inside it is a doorway out
    /// of the root. Refused by name, so the reason is readable.
    #[test]
    fn a_raw_jsonl_that_is_a_symlink_is_refused() {
        let root = scratch("leak");
        let outside = scratch("leak-outside");
        std::fs::write(outside.join("raw.jsonl"), raw_line("room", 0) + "\n").unwrap();
        let leak = root.join("leak");
        std::fs::create_dir_all(&leak).unwrap();
        std::fs::write(leak.join("session.json"), meta("leak")).unwrap();
        std::os::unix::fs::symlink(outside.join("raw.jsonl"), leak.join("raw.jsonl")).unwrap();

        let (got, is_error) = payload(&call_tool(&root, "transcript", r#"{"session":"leak"}"#));
        assert!(is_error, "{got}");
        assert!(got.as_str().unwrap().contains("refusing symlink"), "{got}");
    }

    /// The four things that can be happening to a session, and in each of them
    /// a caller gets an answer rather than an error — a session with no
    /// `raw.jsonl` yet has no lines, which is not a failure.
    #[test]
    fn state_says_what_is_happening_and_a_session_with_no_lines_still_answers() {
        let root = scratch("state");
        transcribed(&root, "done", &[("room", 0)]);
        let live = root.join("live");
        std::fs::create_dir_all(live.join("audio")).unwrap();
        std::fs::write(live.join("session.json"), meta("live")).unwrap();
        std::fs::write(live.join("audio").join("room.native.wav"), b"RIFF....").unwrap();
        let pending = root.join("pending");
        std::fs::create_dir_all(&pending).unwrap();
        std::fs::write(pending.join("session.json"), meta("pending")).unwrap();
        let busy = root.join("busy");
        std::fs::create_dir_all(&busy).unwrap();
        std::fs::write(busy.join("session.json"), meta("busy")).unwrap();
        std::fs::write(
            busy.join(session::TRANSCRIBING_LOCK),
            std::process::id().to_string(),
        )
        .unwrap();

        for (id, state) in [
            ("done", "done"),
            ("live", "live"),
            ("pending", "pending"),
            ("busy", "transcribing"),
        ] {
            let args = format!(r#"{{"session":"{id}"}}"#);
            let (got, is_error) = payload(&call_tool(&root, "transcript", &args));
            assert!(!is_error, "{id} -> {got}");
            assert_eq!(got["state"], state, "{id} -> {got}");
        }
        for id in ["live", "pending", "busy"] {
            let args = format!(r#"{{"session":"{id}"}}"#);
            let (got, _) = payload(&call_tool(&root, "transcript", &args));
            assert_eq!(got["lines"], json!([]), "{id} has no raw.jsonl yet");
            assert_eq!(got["next"], 0, "{id} -> {got}");
        }
    }
}
