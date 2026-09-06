//! Six ways to get a session's words out of it: the existing markdown
//! document unchanged, plain text, JSON, the two subtitle formats, and a copy
//! shaped for pasting into an assistant.
//!
//! Every format but `Markdown` reads [`session::transcript`] directly rather
//! than [`session::markdown`]'s bleed-deduped view: `dedup_bleed` drops room
//! lines it judges to be the microphone overhearing the call, which is right
//! for a document meant to be read once but wrong for a transcript meant to
//! be exact.

use crate::session::{self, SessionMeta, Track};
use anyhow::{bail, Result};
use std::path::Path;

/// Named once so `FromStr`'s error, the `export` API method and the CLI's
/// `bail!` all say the same six words.
const FORMAT_NAMES: &str = "markdown, text, json, srt, vtt, assistant";

/// One of the shapes [`render`] can produce.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Format {
    Markdown,
    Text,
    Json,
    Srt,
    Vtt,
    Assistant,
}

impl std::str::FromStr for Format {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "markdown" => Ok(Format::Markdown),
            "text" => Ok(Format::Text),
            "json" => Ok(Format::Json),
            "srt" => Ok(Format::Srt),
            "vtt" => Ok(Format::Vtt),
            "assistant" => Ok(Format::Assistant),
            other => bail!("unknown export format {other:?}, expected one of: {FORMAT_NAMES}"),
        }
    }
}

/// A session as `format` wants it.
pub fn render(dir: &Path, format: Format) -> Result<String> {
    match format {
        Format::Markdown => session::markdown(dir),
        Format::Text => text(dir),
        Format::Json => json(dir),
        Format::Srt => srt(dir),
        Format::Vtt => vtt(dir),
        Format::Assistant => assistant(dir),
    }
}

/// `speaker: text` per line, a bare `text` when there is no speaker, and a
/// blank line wherever the track changes.
fn text(dir: &Path) -> Result<String> {
    let lines = session::transcript(dir, false)?;
    let mut out = String::new();
    let mut prev_track: Option<Track> = None;
    for l in &lines {
        if prev_track.is_some_and(|t| t != l.track) {
            out.push('\n');
        }
        match &l.speaker {
            Some(name) => out.push_str(&format!("{name}: {}\n", l.text)),
            None => out.push_str(&format!("{}\n", l.text)),
        }
        prev_track = Some(l.track);
    }
    Ok(out)
}

fn json(dir: &Path) -> Result<String> {
    let lines = session::transcript(dir, false)?;
    Ok(serde_json::to_string_pretty(&lines)?)
}

/// `HH:MM:SS,mmm`, the SRT timestamp format.
fn timestamp_srt(ms: u64) -> String {
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        (ms % 3_600_000) / 60_000,
        (ms % 60_000) / 1000,
        ms % 1000
    )
}

/// `HH:MM:SS.mmm`, the VTT timestamp format — the same clock, `.` instead of
/// `,` before the milliseconds.
fn timestamp_vtt(ms: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        (ms % 3_600_000) / 60_000,
        (ms % 60_000) / 1000,
        ms % 1000
    )
}

fn cue_text(speaker: &Option<String>, text: &str) -> String {
    match speaker {
        Some(name) => format!("{name}: {text}"),
        None => text.to_string(),
    }
}

fn srt(dir: &Path) -> Result<String> {
    let lines = session::transcript(dir, false)?;
    let cues: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            format!(
                "{}\n{} --> {}\n{}\n",
                i + 1,
                timestamp_srt(l.start_ms),
                timestamp_srt(l.end_ms),
                cue_text(&l.speaker, &l.text)
            )
        })
        .collect();
    Ok(cues.join("\n"))
}

fn vtt(dir: &Path) -> Result<String> {
    let lines = session::transcript(dir, false)?;
    let cues: Vec<String> = lines
        .iter()
        .map(|l| {
            format!(
                "{} --> {}\n{}\n",
                timestamp_vtt(l.start_ms),
                timestamp_vtt(l.end_ms),
                cue_text(&l.speaker, &l.text)
            )
        })
        .collect();
    Ok(format!("WEBVTT\n\n{}", cues.join("\n")))
}

/// `session::markdown`'s output with everything up to and including its
/// first line starting with `"# "` removed — the front-matter block and the
/// title it generates, so a new heading can be prepended without the result
/// carrying two.
fn strip_first_heading(md: &str) -> &str {
    let mut consumed = 0;
    for line in md.split_inclusive('\n') {
        consumed += line.len();
        if line.trim_end_matches('\n').starts_with("# ") {
            return &md[consumed..];
        }
    }
    md
}

/// `MM:SS` for a duration given in seconds, matching `session::mmss`'s floor.
fn mmss(duration_s: f64) -> String {
    let total = duration_s.max(0.0) as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// The pinned shape for pasting a session into an assistant: a heading naming
/// the session, then `session::markdown`'s body with its own title cut out,
/// then a line pointing at the live MCP server. With no readable
/// `session.json` the heading is just the directory's name, with no
/// parenthetical, and that name doubles as the id in the trailer.
fn assistant(dir: &Path) -> Result<String> {
    let md = session::markdown(dir)?;
    let body = strip_first_heading(&md).trim_start_matches('\n');

    let meta: Option<SessionMeta> = std::fs::read_to_string(dir.join("session.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let dir_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let (heading, id) = match &meta {
        Some(m) => {
            let label = m.name.clone().unwrap_or_else(|| m.id.clone());
            (
                format!("{label} ({}, {})", m.started_at, mmss(m.duration_s)),
                m.id.clone(),
            )
        }
        None => (dir_name.clone(), dir_name),
    };

    Ok(format!(
        "# {heading}\n\n{body}Read live with the ambient MCP server: session id {id}\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session dir with a room line (0-1500ms, "hello") named "Ana" via
    /// `edits.jsonl`, and a call line (1500-4000ms, "hi") with no speaker —
    /// the fixture every format test shares.
    fn fixture(test: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ambient-{test}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("raw.jsonl"),
            "{\"track\":\"room\",\"start_ms\":0,\"end_ms\":1500,\"text\":\"hello\",\"confidence\":0.9}\n\
             {\"track\":\"call\",\"start_ms\":1500,\"end_ms\":4000,\"text\":\"hi\",\"confidence\":0.9}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("edits.jsonl"),
            "{\"kind\":\"speaker\",\"target\":{\"track\":\"room\",\"start_ms\":0},\"name\":\"Ana\",\"by\":\"test\",\"at\":\"2026-09-05T12:00:00Z\"}\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn srt_matches_the_pinned_string() {
        let dir = fixture("export-srt");
        let got = render(&dir, Format::Srt).unwrap();
        assert_eq!(
            got,
            "1\n00:00:00,000 --> 00:00:01,500\nAna: hello\n\n2\n00:00:01,500 --> 00:00:04,000\nhi\n"
        );
    }

    #[test]
    fn vtt_starts_with_the_header_and_uses_dots() {
        let dir = fixture("export-vtt");
        let got = render(&dir, Format::Vtt).unwrap();
        assert!(got.starts_with("WEBVTT\n\n"), "{got}");
        assert!(got.contains("00:00:00.000 --> 00:00:01.500"), "{got}");
    }

    #[test]
    fn json_round_trips_the_two_lines() {
        let dir = fixture("export-json");
        let got = render(&dir, Format::Json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&got).unwrap();
        let lines = value.as_array().unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["text"], "hello");
        assert_eq!(lines[0]["track"], "room");
        assert_eq!(lines[0]["start_ms"], 0);
        assert_eq!(lines[1]["text"], "hi");
    }

    #[test]
    fn an_unknown_format_names_all_six() {
        let err = "xml".parse::<Format>().unwrap_err();
        let message = format!("{err}");
        for name in ["markdown", "text", "json", "srt", "vtt", "assistant"] {
            assert!(message.contains(name), "{message}");
        }
    }
}
