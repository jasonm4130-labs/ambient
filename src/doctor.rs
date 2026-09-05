//! `ambient doctor` — one pass over everything a recording needs, saying
//! which piece is missing rather than which call failed.
//!
//! The verbs already check their own preconditions, but each of them stops at
//! the first failure and only ever looks at what it needs: `record` never
//! notices the diarisation models, and nothing at all notices a config file
//! the loader has quietly fallen back from. This is the one place that asks
//! every question and reports all the answers.
//!
//! Every input is a path or an already-resolved `Result`, passed in by the
//! CLI arm, so the whole thing runs against a temporary directory in a test.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use crate::config::Config;
use crate::session::{self, TRANSCRIBING_LOCK};

/// One question and its answer. `detail` is what makes the answer actionable —
/// the path that is missing, the parse error, the count — so it is filled in
/// on success too.
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
/// folder. Never fails: a check that cannot be answered is a failed check,
/// because the point is to report all of them.
///
/// `models` is a `Result` rather than a path because "there is no models
/// directory" is itself the first check, and the four that follow have to say
/// so rather than guess at a location.
pub fn run(models: Result<PathBuf>, config_file: &Path, sessions: &Path) -> Vec<Check> {
    let mut checks = Vec::new();

    match &models {
        Ok(root) => {
            checks.push(check("models/root", true, root.display().to_string()));
            let files = session::model_files(root);
            for (name, path, is_dir) in [
                ("models/asr", files.asr_dir, true),
                ("models/vad", files.vad, false),
                ("models/segmentation", files.segmentation, false),
                ("models/embedding", files.embedding, false),
            ] {
                let there = if is_dir {
                    path.is_dir()
                } else {
                    path.is_file()
                };
                let detail = if there {
                    path.display().to_string()
                } else {
                    format!("missing {} — run ./fetch-models.sh", path.display())
                };
                checks.push(check(name, there, detail));
            }
        }
        Err(e) => {
            checks.push(check("models/root", false, e.to_string()));
            for name in [
                "models/asr",
                "models/vad",
                "models/segmentation",
                "models/embedding",
            ] {
                checks.push(check(name, false, "no models directory"));
            }
        }
    }

    checks.push(config_check(config_file));
    checks.push(writable(sessions));

    let awaiting = session::captured_awaiting_transcript(sessions).len();
    checks.push(check(
        "sessions/awaiting-transcript",
        true,
        format!("{awaiting} session{}", if awaiting == 1 { "" } else { "s" }),
    ));

    let live = session::live_session_in(sessions);
    checks.push(check(
        "sessions/live",
        true,
        live.map_or_else(|| "none".into(), |d| d.display().to_string()),
    ));

    checks.push(stale_locks(sessions));
    checks
}

/// Read and parse the config here rather than through [`Config::load_from`],
/// which deliberately swallows both failures and returns the defaults — the
/// right thing for a recording in progress, and the reason nobody has ever
/// been told their config file is being ignored.
fn config_check(p: &Path) -> Check {
    let text = match std::fs::read_to_string(p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return check("config", true, "defaults")
        }
        Err(e) => return check("config", false, format!("could not be read: {e}")),
    };
    match serde_json::from_str::<Config>(&text) {
        Ok(_) => check("config", true, p.display().to_string()),
        Err(e) => check("config", false, format!("not valid: {e}")),
    }
}

/// Proved by writing, not by reading a permission bit: the sessions folder can
/// be on a volume that is mounted read-only or not mounted at all, and both
/// look fine until the first recording tries to claim a directory.
fn writable(sessions: &Path) -> Check {
    let probe = sessions.join(format!(".ambient-doctor-{}", std::process::id()));
    match std::fs::write(&probe, "") {
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

/// A lock naming a pid that no longer exists is a transcriber that died. The
/// re-queue takes such a lock over on its own, so this is not a failure the
/// user has to fix — but it is the visible trace of a crash, and a session
/// sitting untranscribed with no explanation is exactly what `doctor` is for.
fn stale_locks(sessions: &Path) -> Check {
    let stale: Vec<String> = session::list(sessions)
        .into_iter()
        .filter_map(|dir| {
            let lock = dir.join(TRANSCRIBING_LOCK);
            let pid: Option<u32> = std::fs::read_to_string(&lock).ok()?.trim().parse().ok();
            // Signal 0 delivers nothing and only reports whether the pid
            // exists. A lock that does not even hold a number is stale too.
            let alive = pid.is_some_and(|pid| unsafe { libc::kill(pid as libc::pid_t, 0) } == 0);
            (!alive).then(|| lock.display().to_string())
        })
        .collect();
    if stale.is_empty() {
        check("sessions/stale-lock", true, "none")
    } else {
        check(
            "sessions/stale-lock",
            false,
            format!("left by a crashed transcriber: {}", stale.join(", ")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::path::{Path, PathBuf};

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ambient-{}-{}", name, std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// One session that has been captured and not yet transcribed.
    fn sessions_dir(root: &Path) {
        let s = root.join("2026-09-05T1000");
        std::fs::create_dir_all(s.join("audio")).unwrap();
        std::fs::write(s.join("session.json"), "{}").unwrap();
        std::fs::write(s.join("audio").join("room.wav"), "").unwrap();
    }

    fn find<'a>(checks: &'a [Check], name: &str) -> &'a Check {
        checks
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no check named {name} in {:?}", names(checks)))
    }

    fn names(checks: &[Check]) -> Vec<&str> {
        checks.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn doctor_names_the_models_that_are_missing() {
        let root = tmp("doctor-missing-models");
        let models = root.join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("wespeaker_en_voxceleb_resnet34_LM.onnx"), "").unwrap();
        let config = root.join("config.json");
        std::fs::write(&config, "{}").unwrap();
        let sessions = root.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        sessions_dir(&sessions);

        let checks = run(Ok(models), &config, &sessions);

        assert_eq!(
            names(&checks),
            vec![
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
        assert!(find(&checks, "models/root").ok);
        assert!(!find(&checks, "models/asr").ok);
        assert!(!find(&checks, "models/vad").ok);
        assert!(!find(&checks, "models/segmentation").ok);
        assert!(find(&checks, "models/embedding").ok);
        assert!(find(&checks, "config").ok);
        assert!(find(&checks, "sessions/writable").ok);
        let awaiting = find(&checks, "sessions/awaiting-transcript");
        assert!(awaiting.ok);
        assert_eq!(awaiting.detail, "1 session");
        let live = find(&checks, "sessions/live");
        assert!(live.ok);
        assert_eq!(live.detail, "none");
        assert!(find(&checks, "sessions/stale-lock").ok);

        std::fs::remove_dir_all(&root).ok();
    }

    /// No models directory at all: every model check fails for the one reason,
    /// and the checks that do not depend on it still answer.
    #[test]
    fn doctor_keeps_going_when_there_is_no_models_directory() {
        let root = tmp("doctor-no-models");
        let config = root.join("config.json");
        std::fs::write(&config, "{}").unwrap();
        let sessions = root.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        sessions_dir(&sessions);

        let checks = run(Err(anyhow!("no models")), &config, &sessions);

        let root_check = find(&checks, "models/root");
        assert!(!root_check.ok);
        assert!(
            root_check.detail.contains("no models"),
            "detail was {:?}",
            root_check.detail
        );
        for name in [
            "models/asr",
            "models/vad",
            "models/segmentation",
            "models/embedding",
        ] {
            let c = find(&checks, name);
            assert!(!c.ok, "{name} should fail");
            assert_eq!(c.detail, "no models directory");
        }
        assert!(find(&checks, "config").ok);
        assert!(find(&checks, "sessions/writable").ok);
        assert_eq!(
            find(&checks, "sessions/awaiting-transcript").detail,
            "1 session"
        );
        assert!(find(&checks, "sessions/live").ok);
        assert!(find(&checks, "sessions/stale-lock").ok);

        std::fs::remove_dir_all(&root).ok();
    }

    /// `Config::load_from` swallows both of these, so `doctor` is the only
    /// place that can say a config file is being ignored.
    #[test]
    fn doctor_reports_a_config_the_loader_would_silently_ignore() {
        let root = tmp("doctor-config");
        let sessions = root.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();

        let broken = root.join("broken.json");
        std::fs::write(&broken, r#"{"diarize":"#).unwrap();
        let c = find(
            &run(Err(anyhow!("no models")), &broken, &sessions),
            "config",
        )
        .clone();
        assert!(!c.ok);
        assert!(c.detail.contains("not valid"), "detail was {:?}", c.detail);

        let missing = root.join("nothing-here.json");
        let c = find(
            &run(Err(anyhow!("no models")), &missing, &sessions),
            "config",
        )
        .clone();
        assert!(c.ok);
        assert_eq!(c.detail, "defaults");

        let a_dir = root.join("a-directory");
        std::fs::create_dir_all(&a_dir).unwrap();
        let c = find(&run(Err(anyhow!("no models")), &a_dir, &sessions), "config").clone();
        assert!(!c.ok);
        assert!(
            c.detail.contains("could not be read"),
            "detail was {:?}",
            c.detail
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
