//! `ambient doctor`. Checks take their paths as arguments and never resolve
//! the real ones, so tests cannot reach `~/Documents/Ambient` or the config.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{pipeline, session};

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

pub fn run(
    models: anyhow::Result<PathBuf>,
    engine: anyhow::Result<PathBuf>,
    config_file: &Path,
    sessions: &Path,
) -> Vec<Check> {
    let mut checks = Vec::with_capacity(9);

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
            checks.push(model_check(
                "models/asr",
                &root.join(pipeline::ASR_DIR),
                true,
            ));
            checks.push(model_check(
                "models/diarization",
                &root.join(pipeline::DIAR_DIR),
                true,
            ));
        }
        None => {
            for name in ["models/asr", "models/diarization"] {
                checks.push(check(name, false, "no models directory"));
            }
        }
    }

    checks.push(match engine {
        Ok(p) if p.is_file() => check("engine", true, p.display().to_string()),
        Ok(p) => check("engine", false, format!("{} is not a file", p.display())),
        Err(e) => check("engine", false, e.to_string()),
    });

    // Not `Config::load`, which swallows read and parse failures.
    checks.push(match fs::read_to_string(config_file) {
        Ok(contents) => match serde_json::from_str::<crate::config::Config>(&contents) {
            Ok(_) => check("config", true, config_file.display().to_string()),
            Err(e) => check("config", false, format!("not valid: {e}")),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => check("config", true, "defaults"),
        Err(e) => check("config", false, format!("could not be read: {e}")),
    });

    // `doctor` writes nothing, so writability comes from the permission bit,
    // not a touch-file.
    checks.push(sessions_writable(sessions));

    let awaiting = session::captured_awaiting_transcript(sessions).len();
    let mut awaiting_detail = match awaiting {
        0 => "none".to_string(),
        1 => "1 session".to_string(),
        n => format!("{n} sessions"),
    };
    // Parked sessions wait only on a person, so they fail the check.
    let parked = session::list(sessions)
        .into_iter()
        .filter(|d| {
            !d.join("transcript.md").is_file() && pipeline::attempts(d) >= pipeline::RETRY_CAP
        })
        .count();
    if parked > 0 {
        awaiting_detail.push_str(&format!(
            "; {parked} failed twice and wait for Retry (the window, or `ambient retry`)"
        ));
    }
    checks.push(check(
        "sessions/awaiting-transcript",
        parked == 0,
        awaiting_detail,
    ));

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

fn sessions_writable(sessions: &Path) -> Check {
    let name = "sessions/writable";
    if sessions.exists() && !sessions.is_dir() {
        return check(
            name,
            false,
            format!("{} is not a folder", sessions.display()),
        );
    }
    let Some(existing) = sessions.ancestors().find(|p| p.exists()) else {
        return check(
            name,
            false,
            format!("{} does not exist", sessions.display()),
        );
    };
    if !existing.is_dir() {
        return check(
            name,
            false,
            format!("{} is not a folder", existing.display()),
        );
    }
    match fs::metadata(existing) {
        Ok(meta) if meta.permissions().readonly() => check(
            name,
            false,
            format!("{} is not writable", existing.display()),
        ),
        _ if existing == sessions => check(name, true, sessions.display().to_string()),
        _ => check(
            name,
            true,
            format!("{} (created on first recording)", sessions.display()),
        ),
    }
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
        fs::create_dir_all(models.join(pipeline::DIAR_DIR)).unwrap();
        let engine = root.join("ambient-engine");
        fs::write(&engine, "").unwrap();

        let config_file = root.join("config.json");
        fs::write(&config_file, "{}").unwrap();

        let sessions = root.join("sessions");
        let session_dir = sessions.join("2026-01-01T00-00-00");
        fs::create_dir_all(session_dir.join("audio")).unwrap();
        fs::write(session_dir.join("session.json"), "{}").unwrap();
        fs::write(session_dir.join("audio").join("room.wav"), "").unwrap();

        let checks = run(Ok(models.clone()), Ok(engine), &config_file, &sessions);

        let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "models/root",
                "models/asr",
                "models/diarization",
                "engine",
                "config",
                "sessions/writable",
                "sessions/awaiting-transcript",
                "sessions/live",
                "sessions/stale-lock",
            ]
        );

        assert!(find(&checks, "models/root").ok);
        assert!(!find(&checks, "models/asr").ok);
        assert!(find(&checks, "models/diarization").ok);
        assert!(find(&checks, "engine").ok);
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

        let checks = run(
            Err(anyhow!("no models")),
            Err(anyhow!("no ambient-engine beside it")),
            &config_file,
            &sessions,
        );

        let models_root = find(&checks, "models/root");
        assert!(!models_root.ok);
        assert!(
            models_root.detail.contains("no models"),
            "detail was: {}",
            models_root.detail
        );

        for name in ["models/asr", "models/diarization"] {
            let c = find(&checks, name);
            assert!(!c.ok, "{name} should FAIL");
            assert_eq!(c.detail, "no models directory");
        }

        let engine = find(&checks, "engine");
        assert!(!engine.ok);
        assert!(
            engine.detail.contains("no ambient-engine"),
            "{}",
            engine.detail
        );
        assert_eq!(checks.len(), 9);
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

        let bad_config = root.join("bad-config.json");
        fs::write(&bad_config, "{\"diarize\":").unwrap();
        let checks = run(
            Ok(models.clone()),
            Err(anyhow!("no helper")),
            &bad_config,
            &sessions,
        );
        let config = find(&checks, "config");
        assert!(!config.ok);
        assert!(
            config.detail.contains("not valid"),
            "detail was: {}",
            config.detail
        );

        let missing_config = root.join("does-not-exist.json");
        let checks = run(
            Ok(models.clone()),
            Err(anyhow!("no helper")),
            &missing_config,
            &sessions,
        );
        let config = find(&checks, "config");
        assert!(config.ok);
        assert_eq!(config.detail, "defaults");

        let dir_config = root.join("config-dir");
        fs::create_dir_all(&dir_config).unwrap();
        let checks = run(
            Ok(models),
            Err(anyhow!("no helper")),
            &dir_config,
            &sessions,
        );
        let config = find(&checks, "config");
        assert!(!config.ok);
        assert!(
            config.detail.contains("could not be read"),
            "detail was: {}",
            config.detail
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn d_fresh_install_without_a_sessions_folder_passes() {
        let root = temp_root("d_fresh_install_without_a_sessions_folder_passes");
        let config_file = root.join("config.json");
        let sessions = root.join("Documents").join("Ambient");

        let checks = run(
            Ok(root.join("models")),
            Err(anyhow!("no helper")),
            &config_file,
            &sessions,
        );

        let writable = find(&checks, "sessions/writable");
        assert!(writable.ok, "detail was: {}", writable.detail);
        assert!(writable.detail.contains("created on first recording"));
        assert!(!sessions.exists(), "doctor must not create the folder");
        assert!(find(&checks, "sessions/awaiting-transcript").ok);
        assert!(find(&checks, "sessions/live").ok);
        assert!(find(&checks, "sessions/stale-lock").ok);

        let blocked = root.join("blocker");
        fs::write(&blocked, "").unwrap();
        let checks = run(
            Ok(root.join("models")),
            Err(anyhow!("no helper")),
            &config_file,
            &blocked.join("Ambient"),
        );
        assert!(!find(&checks, "sessions/writable").ok);

        fs::remove_dir_all(&root).unwrap();
    }
}
