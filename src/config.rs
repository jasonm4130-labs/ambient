//! Settings that outlive a single run.
//!
//! Kept in `~/Library/Application Support/Ambient/config.json` rather than
//! under the sessions folder, because the sessions folder is itself a setting
//! and a config that lives inside the thing it configures cannot be found
//! before it is read.
//!
//! Only settings with something behind them live here. A stored value that no
//! code reads is a promise the app does not keep.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Bundle IDs to tap. Empty means everything the Mac plays.
    pub apps: Vec<String>,
    /// Input device by name. `None` follows the system default.
    pub input_device: Option<String>,
    /// Separate voices once the transcript exists.
    pub diarize: bool,
    /// How readily two utterances are called different people.
    pub threshold: f32,
    /// Where sessions are written. `None` means `~/Documents/Ambient`.
    pub sessions_dir: Option<PathBuf>,
    /// Wait to be told before recording a call this app noticed. Defaults on:
    /// a recording nobody sanctioned is the behaviour that gets a tool banned.
    pub ask_before_recording: bool,
    /// Days to keep the track audio. `Some(0)` deletes it as soon as the
    /// transcript exists; `None` keeps it forever. The audio is the most
    /// sensitive artefact here and the least useful once the text exists.
    pub audio_retention_days: Option<u32>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            apps: Vec::new(),
            input_device: None,
            diarize: true,
            threshold: crate::diarize::DEFAULT_THRESHOLD,
            sessions_dir: None,
            ask_before_recording: true,
            audio_retention_days: Some(7),
        }
    }
}

/// The config file, honouring `AMBIENT_CONFIG` so a test can point somewhere
/// harmless without touching the real one.
pub fn path() -> PathBuf {
    if let Ok(p) = std::env::var("AMBIENT_CONFIG") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Ambient")
        .join("config.json")
}

impl Config {
    /// Never fails. A missing file is the ordinary first run; an unreadable one
    /// says so on stderr and falls back, because losing a recording over a
    /// malformed preference would be absurd.
    pub fn load() -> Self {
        Self::load_from(&path())
    }

    pub fn load_from(p: &Path) -> Self {
        let text = match std::fs::read_to_string(p) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                eprintln!(
                    "  WARNING: {} could not be read ({e}) — using defaults",
                    p.display()
                );
                return Self::default();
            }
        };
        match serde_json::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "  WARNING: {} is not valid config ({e}) — using defaults. \
                     Delete it to start clean.",
                    p.display()
                );
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&path())
    }

    pub fn save_to(&self, p: &Path) -> Result<()> {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(p, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Set one field from a string, for `ambient config <key> <value>`. Listing
    /// the keys here rather than reflecting over the struct keeps the CLI and
    /// the window honest about which settings actually exist.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "apps" => {
                self.apps = value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            }
            "input_device" => {
                self.input_device =
                    (!value.is_empty() && value != "default").then(|| value.to_string())
            }
            "diarize" => self.diarize = flag(value)?,
            "threshold" => self.threshold = value.parse()?,
            "sessions_dir" => {
                self.sessions_dir =
                    (!value.is_empty() && value != "default").then(|| PathBuf::from(value))
            }
            "ask_before_recording" => self.ask_before_recording = flag(value)?,
            // "forever" is spelled out rather than expressed as a large number,
            // so that keeping audio indefinitely is a deliberate word.
            "audio_retention_days" => {
                self.audio_retention_days = match value {
                    "forever" | "never" => None,
                    v => Some(v.parse()?),
                }
            }
            other => anyhow::bail!(
                "unknown setting {other:?}. Known: apps, input_device, diarize, \
                 threshold, sessions_dir, ask_before_recording, audio_retention_days"
            ),
        }
        Ok(())
    }
}

/// Refuse a setting change that a running capture would make incoherent.
///
/// Only `sessions_dir` is at stake, and only while something is recording:
/// moving it then splits one conversation across two folders for nothing, since
/// the capture keeps writing into the directory it claimed at the start. The
/// window already said no; saying it here too means the CLI cannot walk past a
/// rule the GUI enforces.
pub fn refuse_while_live(key: &str, live: Option<&Path>) -> Result<()> {
    if key == "sessions_dir" {
        if let Some(dir) = live {
            anyhow::bail!(
                "a recording is in progress in {}; stop it before moving the \
                 sessions directory",
                dir.display()
            );
        }
    }
    Ok(())
}

/// Parse a boolean setting, refusing anything it does not recognise.
///
/// Treating every unrecognised word as `false` would mean `config diarize maybe`
/// silently turns diarization off and reports success — the silent fallback this
/// project keeps having to remove, reintroduced in the one layer that must not
/// have it.
fn flag(value: &str) -> Result<bool> {
    match value {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        other => anyhow::bail!("{other:?} is not a yes or no. Use true or false."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("ambient-cfg-{name}-{}.json", std::process::id()));
        std::fs::remove_file(&p).ok();
        p
    }

    #[test]
    fn a_missing_file_is_an_ordinary_first_run() {
        let p = temp("missing");
        assert_eq!(Config::load_from(&p), Config::default());
    }

    #[test]
    fn an_unrecognised_boolean_is_refused_rather_than_read_as_no() {
        let mut c = Config {
            diarize: true,
            ..Config::default()
        };
        assert!(c.set("diarize", "maybe").is_err());
        assert!(c.diarize, "a rejected value must leave the setting alone");
        c.set("diarize", "off").unwrap();
        assert!(!c.diarize);
    }

    #[test]
    fn a_corrupt_file_falls_back_rather_than_failing() {
        let p = temp("corrupt");
        std::fs::write(&p, "{ this is not json").unwrap();
        assert_eq!(Config::load_from(&p), Config::default());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn an_unknown_field_does_not_discard_the_known_ones() {
        // Config written by a newer build must still load in an older one.
        let p = temp("extra");
        std::fs::write(&p, r#"{"diarize": false, "invented_later": 7}"#).unwrap();
        assert!(!Config::load_from(&p).diarize);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn a_partial_file_keeps_the_defaults_for_everything_else() {
        let p = temp("partial");
        std::fs::write(&p, r#"{"apps": ["com.apple.Safari"]}"#).unwrap();
        let c = Config::load_from(&p);
        assert_eq!(c.apps, ["com.apple.Safari"]);
        assert_eq!(c.threshold, Config::default().threshold);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn it_round_trips_through_the_file() {
        let p = temp("roundtrip");
        let mut c = Config::default();
        c.set("apps", "com.apple.Safari, us.zoom.xos").unwrap();
        c.set("diarize", "false").unwrap();
        c.set("threshold", "0.55").unwrap();
        c.set("input_device", "MacBook Pro Microphone").unwrap();
        c.save_to(&p).unwrap();
        assert_eq!(Config::load_from(&p), c);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn default_clears_an_optional_setting() {
        let mut c = Config::default();
        c.set("input_device", "Iriun Webcam Audio").unwrap();
        assert!(c.input_device.is_some());
        c.set("input_device", "default").unwrap();
        assert_eq!(c.input_device, None);
    }

    #[test]
    fn keeping_audio_forever_is_a_word_not_a_big_number() {
        let mut c = Config::default();
        c.set("audio_retention_days", "forever").unwrap();
        assert_eq!(c.audio_retention_days, None);
        c.set("audio_retention_days", "0").unwrap();
        assert_eq!(c.audio_retention_days, Some(0));
    }

    #[test]
    fn moving_the_sessions_folder_mid_recording_is_refused() {
        let e = refuse_while_live("sessions_dir", Some(Path::new("/x"))).unwrap_err();
        assert!(e.to_string().contains("/x"), "{e}");
        // Nothing is recording, and every other setting is free to change
        // either way: only the folder the capture is writing into is at stake.
        assert!(refuse_while_live("sessions_dir", None).is_ok());
        assert!(refuse_while_live("diarize", Some(Path::new("/x"))).is_ok());
    }

    #[test]
    fn an_unknown_key_is_refused_by_name() {
        let e = Config::default().set("keep_audio", "true").unwrap_err();
        assert!(e.to_string().contains("keep_audio"), "{e}");
    }
}
