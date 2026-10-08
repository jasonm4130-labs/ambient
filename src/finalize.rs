//! Finalization in a short-lived child process.
//!
//! [`crate::session::transcribe_session`] loads the recogniser and the
//! diarizer, and after it returns macOS's allocator keeps the pages it freed:
//! a 25-minute meeting left the process at over 1 GiB resident and 430–690 MiB
//! of footprint (`docs/developing/measurements.md`) with 13 MB of heap live,
//! and `malloc_zone_pressure_relief` returned none of it. A process that
//! exits returns everything, so the app's queue runs each job as
//! `ambient finalize <session-dir>` — the same signed executable, so no new
//! binary or entitlement — and waits for it.
//!
//! What crosses the boundary is small and line-oriented on the child's
//! stdout: `phase <n>` whenever the [`Meter`] moves, and one `error <message>`
//! if the job fails. Its stderr is the parent's, as the in-process job's was.
//! The child reads its stdin until end of file and exits when it gets there,
//! so a parent that dies takes its job with it, exactly as the in-process job
//! died with the app. Either way the session is left as a crash leaves it: no
//! live lock, `session.json` and audio on disk, re-queued at the next launch.

use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use crate::session::{Meter, MeterPhase, STATUS_FILE, TRANSCRIBING_LOCK};

/// The subcommand the child runs under.
pub const SUBCOMMAND: &str = "finalize";

/// `exe finalize <dir>`, with its pipes set up and the decoder lane shared if
/// this process has one. Returned unspawned so a caller can add to its
/// environment.
pub fn command(exe: &Path, dir: &Path) -> Command {
    let mut cmd = Command::new(exe);
    cmd.arg(SUBCOMMAND)
        .arg(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(lane) = crate::live::shared_decoder() {
        cmd.env(crate::live::DECODER_ENV, lane);
    }
    cmd
}

/// Run `cmd` (from [`command`]) to completion, mirroring its progress onto
/// `meter`, and answer as [`crate::session::transcribe_session`] would have.
pub fn run(mut cmd: Command, dir: &Path, meter: &Arc<Meter>) -> Result<PathBuf> {
    let result = match cmd.spawn() {
        Ok(mut child) => {
            let r = supervise(&mut child, meter);
            if r.is_err() {
                // A child that crashed left its lock behind, naming a pid
                // that is gone; one that reported its own failure did not.
                release_lock(dir, child.id());
            }
            r
        }
        Err(e) => Err(anyhow!(e).context("starting the transcription process")),
    };
    if let Err(e) = &result {
        // Rewritten even when the child wrote it, so a crash ends exactly
        // where an in-process failure did.
        std::fs::write(dir.join(STATUS_FILE), format!("failed: {e:#}\n")).ok();
        meter.set_phase(MeterPhase::Failed);
    }
    result.map(|()| dir.to_path_buf())
}

fn supervise(child: &mut Child, meter: &Arc<Meter>) -> Result<()> {
    // Held, never written: closing it is how the child learns the parent is
    // gone. It closes when this function returns, after the child has exited.
    let _stdin = child.stdin.take();
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut error = None;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if let Some(n) = line.strip_prefix("phase ") {
            if let Ok(n) = n.trim().parse::<u8>() {
                meter.set_phase(MeterPhase::from_u8(n));
            }
        } else if let Some(msg) = line.strip_prefix("error ") {
            error = Some(msg.to_string());
        }
    }
    let status = child
        .wait()
        .context("waiting for the transcription process")?;
    match (status.success(), error) {
        (true, _) => Ok(()),
        (false, Some(msg)) => Err(anyhow!(msg)),
        (false, None) => Err(anyhow!("the transcription process {}", describe(status))),
    }
}

fn describe(status: ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (_, Some(sig)) => format!("was killed by signal {sig}"),
        (Some(code), _) => format!("exited with status {code} without saying why"),
        _ => "ended without an exit status".to_string(),
    }
}

/// Remove the session's lock if it still names `pid`. Once the child is
/// reaped its pid can be reused, and a lock naming a live stranger would keep
/// the session from ever being re-queued.
fn release_lock(dir: &Path, pid: u32) {
    let path = dir.join(TRANSCRIBING_LOCK);
    if std::fs::read_to_string(&path).is_ok_and(|s| s.trim() == pid.to_string()) {
        std::fs::remove_file(&path).ok();
    }
    std::fs::remove_file(dir.join(format!("{TRANSCRIBING_LOCK}.{pid}"))).ok();
}

/// The child: `ambient finalize <dir>`. Prints the protocol [`run`] reads and
/// exits non-zero on failure.
pub fn serve(dir: &Path) -> Result<()> {
    if let Some(lane) = std::env::var_os(crate::live::DECODER_ENV) {
        crate::live::share_decoder(PathBuf::from(lane));
    }
    std::thread::spawn(|| {
        // Only end of file or an error gets past this: the parent never
        // writes. Exiting here abandons the job mid-write, which the
        // session's journal already survives — it is what a crash does.
        let _ = std::io::stdin().lock().read_to_end(&mut Vec::new());
        std::process::exit(2);
    });
    let meter = Arc::new(Meter::default());
    let job = {
        let (dir, meter) = (dir.to_path_buf(), meter.clone());
        std::thread::spawn(move || {
            // The same arrangement as the queue's thread in the app: the job
            // on a background-QoS thread, so it yields to a live capture's
            // drain loop, while this process's main thread stays at its
            // default to relay progress.
            #[cfg(target_os = "macos")]
            unsafe {
                libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
            }
            crate::session::transcribe_session(&dir, None, Some(meter))
        })
    };
    let mut last = None;
    loop {
        let finished = job.is_finished();
        let phase = meter.phase();
        // `Capturing` is only the meter's default; this job never captures,
        // and the parent's meter has already left it.
        if phase != MeterPhase::Capturing && last != Some(phase) {
            println!("phase {}", phase as u8);
            let _ = std::io::stdout().flush();
            last = Some(phase);
        }
        if finished {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let result = job
        .join()
        .map_err(|_| anyhow!("the transcription thread panicked"))
        .and_then(|r| r);
    if let Err(e) = &result {
        // One line: the parent reads it as the whole message.
        println!("error {}", format!("{e:#}").replace('\n', " "));
        let _ = std::io::stdout().flush();
    }
    result.map(|_| ())
}
