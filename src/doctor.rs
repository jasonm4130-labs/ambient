//! What is missing, all of it, in one pass.
//!
//! Every other verb stops at its first missing model or unreadable file, which
//! makes a broken install a sequence of one-line failures fixed one at a time.
//! `doctor` reports every check regardless of what the earlier ones said —
//! including the sessions folder when there are no models at all, since those
//! two halves fail for unrelated reasons.
//!
//! Nothing here loads a model or opens the audio hardware: it is a filesystem
//! report, cheap enough to run before every bug report. `ambient probe` is the
//! one that talks to Core Audio.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use crate::config::Config;
use crate::session::{self, TRANSCRIBING_LOCK};

/// One line of the report. `detail` is the whole explanation — a check that
/// fails without saying which path it looked at has told the reader nothing.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

fn check(name: &str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        ok,
        detail: detail.into(),
    }
}

/// Run every check against the given models root, config file and sessions
/// folder, in report order. Takes the three locations rather than finding them,
/// so the tests can point it at a temp directory — and so a `models_root` that
/// failed is reportable as a check of its own instead of an early return.
pub fn run(models: Result<PathBuf>, config_file: &Path, sessions: &Path) -> Vec<Check> {
    let mut out = Vec::new();

    match &models {
        Ok(root) => {
            // `AMBIENT_MODELS` can name a directory that is not there; the
            // root is only ok when it exists, or every model check below
            // fails for a reason this line has just called fine.
            let there = root.is_dir();
            out.push(check(
                "models/root",
                there,
                if there {
                    root.display().to_string()
                } else {
                    format!("{} does not exist", root.display())
                },
            ));
            let f = session::model_files(root);
            // The ASR model is a directory of tensors and configs; the other
            // three are single files.
            out.push(present("models/asr", &f.asr_dir, f.asr_dir.is_dir()));
            out.push(present("models/vad", &f.vad, f.vad.is_file()));
            out.push(present(
                "models/segmentation",
                &f.segmentation,
                f.segmentation.is_file(),
            ));
            out.push(present(
                "models/embedding",
                &f.embedding,
                f.embedding.is_file(),
            ));
        }
        Err(e) => {
            out.push(check("models/root", false, e.to_string()));
            for name in [
                "models/asr",
                "models/vad",
                "models/segmentation",
                "models/embedding",
            ] {
                out.push(check(name, false, "no models directory"));
            }
        }
    }

    out.push(config(config_file));
    out.push(writable(sessions));

    let awaiting = session::captured_awaiting_transcript(sessions).len();
    out.push(check(
        "sessions/awaiting-transcript",
        true,
        match awaiting {
            0 => "none".to_string(),
            1 => "1 session".to_string(),
            n => format!("{n} sessions"),
        },
    ));

    out.push(check(
        "sessions/live",
        true,
        match session::live_session_in(sessions) {
            Some(d) => d.display().to_string(),
            None => "none".to_string(),
        },
    ));

    out.push(stale_locks(sessions));
    out
}

fn present(name: &str, path: &Path, ok: bool) -> Check {
    let detail = if ok {
        path.display().to_string()
    } else {
        format!("missing {} — run ./fetch-models.sh", path.display())
    };
    check(name, ok, detail)
}

/// Read and parse the config here rather than through [`Config::load_from`],
/// which deliberately swallows both failures so a malformed preference cannot
/// cost a recording. That is right for recording and wrong for a report whose
/// entire job is to say what is broken.
fn config(path: &Path) -> Check {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        // No file is the ordinary first run, not a fault.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return check("config", true, "defaults")
        }
        Err(e) => {
            return check(
                "config",
                false,
                format!("{} could not be read: {e}", path.display()),
            )
        }
    };
    match serde_json::from_str::<Config>(&text) {
        Ok(_) => check("config", true, path.display().to_string()),
        Err(e) => check(
            "config",
            false,
            format!("{} is not valid config: {e}", path.display()),
        ),
    }
}

/// Writable, established by writing — `metadata` permissions would answer for
/// the wrong user on a folder inside an app sandbox or on a mounted volume.
fn writable(sessions: &Path) -> Check {
    let probe = sessions.join(format!(".ambient-doctor-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(sessions) {
        return check(
            "sessions/writable",
            false,
            format!("{} could not be created: {e}", sessions.display()),
        );
    }
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            std::fs::remove_file(&probe).ok();
            check("sessions/writable", true, sessions.display().to_string())
        }
        Err(e) => check(
            "sessions/writable",
            false,
            format!("{} is not writable: {e}", sessions.display()),
        ),
    }
}

/// A lock naming a pid that no longer exists is a crash's leftovers. Nothing
/// removes it until something tries to transcribe that session again, so it is
/// worth naming: it is the reason a session can sit un-transcribed for ever
/// with no error anywhere.
fn stale_locks(sessions: &Path) -> Check {
    let stale: Vec<String> = session::list(sessions)
        .into_iter()
        .filter(|dir| {
            let Some(pid) = read_pid(&dir.join(TRANSCRIBING_LOCK)) else {
                return false;
            };
            // Signal 0 delivers nothing and only reports whether the pid
            // exists. Safe: no memory is involved and a wrong pid is
            // answered, not acted on.
            (unsafe { libc::kill(pid as libc::pid_t, 0) }) != 0
        })
        .map(|dir| dir.join(TRANSCRIBING_LOCK).display().to_string())
        .collect();
    if stale.is_empty() {
        check("sessions/stale-lock", true, "none")
    } else {
        check(
            "sessions/stale-lock",
            false,
            format!("dead transcriber holds {} — delete it", stale.join(", ")),
        )
    }
}

fn read_pid(lock: &Path) -> Option<u32> {
    std::fs::read_to_string(lock).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::path::{Path, PathBuf};

    /// A fresh directory per case, so two cases cannot see each other's files.
    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ambient-doctor-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&p).ok();
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn detail<'a>(checks: &'a [Check], name: &str) -> &'a str {
        &checks
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no check named {name}"))
            .detail
    }

    fn ok(checks: &[Check], name: &str) -> bool {
        checks.iter().find(|c| c.name == name).unwrap().ok
    }

    /// A models dir holding one of the four files, a config, and one session
    /// captured but not transcribed.
    fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = scratch(tag);
        let models = root.join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("wespeaker_en_voxceleb_resnet34_LM.onnx"), b"").unwrap();
        let config = root.join("config.json");
        std::fs::write(&config, b"{}").unwrap();
        let sessions = root.join("sessions");
        let one = sessions.join("2026-09-05T0900");
        std::fs::create_dir_all(one.join("audio")).unwrap();
        std::fs::write(one.join("session.json"), b"{}").unwrap();
        std::fs::write(one.join("audio").join("room.wav"), b"x").unwrap();
        (models, config, sessions)
    }

    #[test]
    fn names_the_three_missing_models_and_leaves_the_rest_ok() {
        let (models, config, sessions) = fixture("missing");
        let checks = run(Ok(models.clone()), &config, &sessions);
        assert_eq!(
            checks.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            [
                "models/root",
                "models/asr",
                "models/vad",
                "models/segmentation",
                "models/embedding",
                "config",
                "sessions/writable",
                "sessions/awaiting-transcript",
                "sessions/live",
                "sessions/stale-lock",
            ]
        );
        assert!(ok(&checks, "models/root"), "the directory is there");
        assert!(!ok(&checks, "models/asr"));
        assert!(!ok(&checks, "models/vad"));
        assert!(!ok(&checks, "models/segmentation"));
        assert!(ok(&checks, "models/embedding"), "the one file that exists");
        assert!(ok(&checks, "config"));
        assert!(ok(&checks, "sessions/writable"));
        assert!(ok(&checks, "sessions/awaiting-transcript"));
        assert_eq!(detail(&checks, "sessions/awaiting-transcript"), "1 session");
        assert!(ok(&checks, "sessions/live"));
        assert_eq!(detail(&checks, "sessions/live"), "none");
        assert!(ok(&checks, "sessions/stale-lock"));
        std::fs::remove_dir_all(models.parent().unwrap()).ok();
    }

    /// No models directory at all: every model check fails for the one reason,
    /// and the sessions half of the report still runs — the whole point of a
    /// doctor is that one broken thing does not hide the others.
    /// `AMBIENT_MODELS` pointing at nothing must not read as a root that is
    /// present: the verb exists to say what is missing.
    #[test]
    fn a_models_root_that_does_not_exist_is_not_ok() {
        let (_, config_file, sessions) = fixture("absent-root");
        let missing = scratch("absent-root-models").join("nowhere");
        let checks = run(Ok(missing.clone()), &config_file, &sessions);
        assert!(!ok(&checks, "models/root"));
        assert!(detail(&checks, "models/root").contains("does not exist"));
        assert!(!ok(&checks, "models/asr"));
    }

    #[test]
    fn a_missing_models_root_fails_all_four_and_still_checks_sessions() {
        let (models, config, sessions) = fixture("noroot");
        let checks = run(Err(anyhow!("no models")), &config, &sessions);
        assert!(!ok(&checks, "models/root"));
        assert!(detail(&checks, "models/root").contains("no models"));
        for name in [
            "models/asr",
            "models/vad",
            "models/segmentation",
            "models/embedding",
        ] {
            assert!(!ok(&checks, name), "{name} cannot be ok");
            assert_eq!(detail(&checks, name), "no models directory");
        }
        assert!(ok(&checks, "config"));
        assert!(ok(&checks, "sessions/writable"));
        assert_eq!(detail(&checks, "sessions/awaiting-transcript"), "1 session");
        std::fs::remove_dir_all(models.parent().unwrap()).ok();
    }

    #[test]
    fn config_distinguishes_absent_from_unparseable_from_unreadable() {
        let root = scratch("config");
        let models: anyhow::Result<PathBuf> = Err(anyhow!("no models"));
        let check = |p: &Path| {
            let checks = run(
                models
                    .as_ref()
                    .map(Clone::clone)
                    .map_err(|e| anyhow!("{e}")),
                p,
                &root,
            );
            let c = checks.into_iter().find(|c| c.name == "config").unwrap();
            (c.ok, c.detail)
        };

        let missing = root.join("absent.json");
        assert_eq!(check(&missing), (true, "defaults".to_string()));

        let broken = root.join("broken.json");
        std::fs::write(&broken, b"{\"diarize\":").unwrap();
        let (ok, detail) = check(&broken);
        assert!(!ok);
        assert!(detail.contains("not valid"), "{detail}");

        let dir = root.join("a-directory.json");
        std::fs::create_dir_all(&dir).unwrap();
        let (ok, detail) = check(&dir);
        assert!(!ok);
        assert!(detail.contains("could not be read"), "{detail}");

        std::fs::remove_dir_all(&root).ok();
    }
}
