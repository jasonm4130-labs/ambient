//! The client side of `ambient-engine`, the model helper (ADR 0020).
//!
//! One call runs one stage over one track in its own process and waits for
//! it. The protocol is the helper's stdout, one event per line:
//! `progress <0..1>` as it goes, and `error <message>` before a non-zero exit.
//! Its stderr is ours. Its stdin is a pipe that we hold and never write to:
//! when this process dies the pipe closes, and the helper exits rather than
//! running on unowned.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

/// Points at a helper binary other than the one beside this executable: a
/// development build, or a stub in tests.
pub const ENGINE_ENV: &str = "AMBIENT_ENGINE";
pub const ENGINE_NAME: &str = "ambient-engine";

/// What the helper can do to a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Transcribe,
    Diarize,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Transcribe => "transcribe",
            Stage::Diarize => "diarize",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptFile {
    pub key: String,
    pub model: String,
    pub duration_seconds: f64,
    pub words: Vec<Word>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakersFile {
    pub key: String,
    pub model: String,
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub speaker: usize,
    pub start: f64,
    pub end: f64,
}

/// The helper to run: `$AMBIENT_ENGINE`, else `ambient-engine` beside this
/// executable, which is where the app bundle puts it.
pub fn locate() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os(ENGINE_ENV) {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe().context("locating this executable")?;
    let beside = exe.with_file_name(ENGINE_NAME);
    if beside.is_file() {
        return Ok(beside);
    }
    bail!(
        "no {ENGINE_NAME} beside {} — build the app with ./make-app.sh, or set \
         {ENGINE_ENV}=$(scripts/build-engine)",
        exe.display()
    )
}

/// Run one stage to completion. `progress` gets each fraction the helper
/// reports.
pub fn run(
    engine: &Path,
    stage: Stage,
    models: &Path,
    audio: &Path,
    out: &Path,
    key: &str,
    mut progress: impl FnMut(f64),
) -> Result<()> {
    let mut child = Command::new(engine)
        .arg(stage.as_str())
        .arg("--models")
        .arg(models)
        .arg("--audio")
        .arg(audio)
        .arg("--out")
        .arg(out)
        .arg("--key")
        .arg(key)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("starting {}", engine.display()))?;
    // Held, never written: closing it is how the helper learns we are gone.
    let _stdin = child.stdin.take();
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut error = None;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if let Some(f) = line.strip_prefix("progress ") {
            if let Ok(f) = f.trim().parse::<f64>() {
                progress(f);
            }
        } else if let Some(msg) = line.strip_prefix("error ") {
            error = Some(msg.to_string());
        }
    }
    let (status, peak) = wait_with_peak(&child)?;
    // One line per stage, for the log and for scripts/memory, which gates on
    // the largest. The helper is where the models' memory lives now.
    eprintln!("ENGINE_PEAK {} {peak}", stage.as_str());
    match (status.success(), error) {
        (true, _) => Ok(()),
        (false, Some(msg)) => Err(anyhow!("{} {}: {msg}", ENGINE_NAME, stage.as_str())),
        (false, None) => Err(anyhow!(
            "{} {} {}",
            ENGINE_NAME,
            stage.as_str(),
            describe(status)
        )),
    }
}

/// Reap the helper and return its exit status with its peak resident set,
/// in bytes. `Child::wait` gives no resource usage, so this is `wait4`.
fn wait_with_peak(child: &std::process::Child) -> Result<(ExitStatus, u64)> {
    use std::os::unix::process::ExitStatusExt;
    let pid = child.id() as libc::pid_t;
    let mut status = 0;
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        let r = unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
        if r == pid {
            break;
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e).context("waiting for the model helper");
        }
    }
    // Bytes on macOS (kilobytes on Linux, where this never runs).
    Ok((ExitStatus::from_raw(status), usage.ru_maxrss.max(0) as u64))
}

fn describe(status: ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (_, Some(sig)) => format!("was killed by signal {sig}"),
        (Some(code), _) => format!("exited with status {code} without saying why"),
        _ => "ended without an exit status".to_string(),
    }
}
