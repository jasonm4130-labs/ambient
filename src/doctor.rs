//! What a person whose Ambient will not record runs first.
//!
//! Ten checks, always in the same order, each pure with respect to the paths
//! it is handed — no check calls `session::home()`, `config::path()` or
//! `session::models_root()` itself, so the tests below never reach the real
//! `~/Documents/Ambient` or the real config file. The CLI arm passes the real
//! roots in.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::session;

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

fn check(name: &str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        name: name.to_string(),
        ok,
        detail: detail.into(),
    }
}

/// A model file or directory under a resolved models root: ok with the path
/// as detail, or FAIL naming what to run to fix it.
fn model_check(name: &str, path: &Path, is_dir: bool) -> Check {
    let present = if is_dir {
        path.is_dir()
    } else {
        path.is_file()
    };
    if present {
        check(name, true, path.display().to_string())
    } else {
        check(
            name,
            false,
            format!("missing {} — run ./fetch-models.sh", path.display()),
        )
    }
}

/// Run the ten checks against the given roots. `models` is the already-resolved
/// (or already-failed) `session::models_root()` result, taken by value so a
/// test can hand in `Err` without touching a real filesystem location.
pub fn run(models: anyhow::Result<PathBuf>, config_file: &Path, sessions: &Path) -> Vec<Check> {
    let mut checks = Vec::with_capacity(10);

    // Collapse to `Option` here so the four model checks below have exactly
    // two code paths: a resolved root, or none.
    let root: Option<PathBuf> = match models {
        Ok(p) if p.is_dir() => {
            checks.push(check("models/root", true, p.display().to_string()));
            Some(p)
        }
        Ok(p) => {
            checks.push(check(
                "models/root",
                false,
                format!("{} is not a directory", p.display()),
            ));
            None
        }
        Err(e) => {
            checks.push(check("models/root", false, e.to_string()));
            None
        }
    };

    match &root {
        Some(root) => {
            let files = session::model_files(root);
            checks.push(model_check("models/asr", &files.asr_dir, true));
            checks.push(model_check("models/vad", &files.vad, false));
            checks.push(model_check(
                "models/segmentation",
                &files.segmentation,
                false,
            ));
            checks.push(model_check("models/embedding", &files.embedding, false));
        }
        None => {
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

    // Read and parse the file directly rather than going through
    // `Config::load`/`Config::load_from`, which swallow both a read failure
    // and a parse failure and print a warning instead.
    checks.push(match fs::read_to_string(config_file) {
        Ok(contents) => match serde_json::from_str::<crate::config::Config>(&contents) {
            Ok(_) => check("config", true, config_file.display().to_string()),
            Err(e) => check("config", false, format!("not valid: {e}")),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => check("config", true, "defaults"),
        Err(e) => check("config", false, format!("could not be read: {e}")),
    });

    // `doctor` writes nothing at all, so this infers writability from the
    // permission bit rather than probing with a touch-file — a weaker check,
    // and a deliberate one.
    checks.push(if !sessions.is_dir() {
        check(
            "sessions/writable",
            false,
            format!("{} does not exist", sessions.display()),
        )
    } else {
        match fs::metadata(sessions) {
            Ok(meta) if meta.permissions().readonly() => check(
                "sessions/writable",
                false,
                format!("{} is not writable", sessions.display()),
            ),
            _ => check("sessions/writable", true, sessions.display().to_string()),
        }
    });

    let awaiting = session::captured_awaiting_transcript(sessions).len();
    let awaiting_detail = match awaiting {
        0 => "none".to_string(),
        1 => "1 session".to_string(),
        n => format!("{n} sessions"),
    };
    checks.push(check("sessions/awaiting-transcript", true, awaiting_detail));

    let live_detail = match session::live_session_in(sessions) {
        Some(dir) => dir.display().to_string(),
        None => "none".to_string(),
    };
    checks.push(check("sessions/live", true, live_detail));

    let stale: Vec<String> = session::list(sessions)
        .into_iter()
        .filter(|dir| {
            dir.join(session::TRANSCRIBING_LOCK).is_file()
                && session::live_transcriber(dir).is_none()
        })
        .map(|dir| dir.display().to_string())
        .collect();
    checks.push(if stale.is_empty() {
        check("sessions/stale-lock", true, "none")
    } else {
        check("sessions/stale-lock", false, stale.join(", "))
    });

    checks
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    fn temp_root(fn_name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ambient-{fn_name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn find<'a>(checks: &'a [Check], name: &str) -> &'a Check {
        checks.iter().find(|c| c.name == name).unwrap_or_else(|| {
            panic!("no check named {name}");
        })
    }

    #[test]
    fn a_mixed_models_config_and_awaiting_session() {
        let root = temp_root("a_mixed_models_config_and_awaiting_session");

        let models = root.join("models");
        fs::create_dir_all(&models).unwrap();
        fs::write(models.join("wespeaker_en_voxceleb_resnet34_LM.onnx"), "").unwrap();

        let config_file = root.join("config.json");
        fs::write(&config_file, "{}").unwrap();

        let sessions = root.join("sessions");
        let session_dir = sessions.join("2026-01-01T00-00-00");
        fs::create_dir_all(session_dir.join("audio")).unwrap();
        fs::write(session_dir.join("session.json"), "{}").unwrap();
        fs::write(session_dir.join("audio").join("room.wav"), "").unwrap();

        let checks = run(Ok(models.clone()), &config_file, &sessions);

        let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
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

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn b_no_models_root_still_runs_the_sessions_checks() {
        let root = temp_root("b_no_models_root_still_runs_the_sessions_checks");
        let config_file = root.join("config.json");
        let sessions = root.join("sessions");
        fs::create_dir_all(&sessions).unwrap();

        let checks = run(Err(anyhow!("no models")), &config_file, &sessions);

        let models_root = find(&checks, "models/root");
        assert!(!models_root.ok);
        assert!(
            models_root.detail.contains("no models"),
            "detail was: {}",
            models_root.detail
        );

        for name in [
            "models/asr",
            "models/vad",
            "models/segmentation",
            "models/embedding",
        ] {
            let c = find(&checks, name);
            assert!(!c.ok, "{name} should FAIL");
            assert_eq!(c.detail, "no models directory");
        }

        // Sessions checks still evaluated despite the models failure.
        assert_eq!(checks.len(), 10);
        assert!(find(&checks, "sessions/awaiting-transcript").ok);
        assert!(find(&checks, "sessions/live").ok);
        assert!(find(&checks, "sessions/stale-lock").ok);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn c_config_invalid_missing_and_a_directory() {
        let root = temp_root("c_config_invalid_missing_and_a_directory");
        let sessions = root.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let models = root.join("models");

        // Invalid JSON.
        let bad_config = root.join("bad-config.json");
        fs::write(&bad_config, "{\"diarize\":").unwrap();
        let checks = run(Ok(models.clone()), &bad_config, &sessions);
        let config = find(&checks, "config");
        assert!(!config.ok);
        assert!(
            config.detail.contains("not valid"),
            "detail was: {}",
            config.detail
        );

        // Missing config file.
        let missing_config = root.join("does-not-exist.json");
        let checks = run(Ok(models.clone()), &missing_config, &sessions);
        let config = find(&checks, "config");
        assert!(config.ok);
        assert_eq!(config.detail, "defaults");

        // Config path that is a directory.
        let dir_config = root.join("config-dir");
        fs::create_dir_all(&dir_config).unwrap();
        let checks = run(Ok(models), &dir_config, &sessions);
        let config = find(&checks, "config");
        assert!(!config.ok);
        assert!(
            config.detail.contains("could not be read"),
            "detail was: {}",
            config.detail
        );

        fs::remove_dir_all(&root).unwrap();
    }
}
