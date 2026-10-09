//! The assistant's voice: which engine to run, and the child process that
//! runs it.
//!
//! Text-to-speech never runs in Ambient's process. The engines that sound
//! human need from 0.4 GB (Kokoro) to 2.7 GB (Qwen3-TTS) of their own, which
//! would break the recorder's memory budget and let a model crash take a
//! recording down with it. Instead `voice/` holds a small Python helper that
//! reads one JSON request per line on stdin and answers with one event per
//! line on stdout; [`Voice`] starts it on the first line, restarts it when it
//! dies, and gives up on speech — not on the assistant — when it keeps dying.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// The local voices the helper can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {
    /// Qwen3-TTS 0.6B on MLX: follows a delivery instruction, streams its
    /// first audio in about 0.1 s, and peaks around 2.7 GB.
    #[serde(rename = "qwen3-tts")]
    Qwen3Tts,
    /// Supertonic 3 on ONNX Runtime: `<laugh>`/`<sigh>` tags, about 0.6 GB.
    #[serde(rename = "supertonic3")]
    Supertonic3,
    /// Kokoro-82M int8 on ONNX Runtime: no emotion, about 0.4 GB.
    #[serde(rename = "kokoro")]
    Kokoro,
}

impl Engine {
    pub fn as_str(self) -> &'static str {
        match self {
            Engine::Qwen3Tts => "qwen3-tts",
            Engine::Supertonic3 => "supertonic3",
            Engine::Kokoro => "kokoro",
        }
    }
}

impl std::fmt::Display for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Engine {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "qwen3-tts" => Ok(Engine::Qwen3Tts),
            "supertonic3" => Ok(Engine::Supertonic3),
            "kokoro" => Ok(Engine::Kokoro),
            other => bail!(
                "{other:?} is not a voice engine. Use auto, qwen3-tts, supertonic3 or kokoro."
            ),
        }
    }
}

const GIB: u64 = 1 << 30;

/// The engine a Mac with this much physical memory gets when the setting is
/// `auto`. Qwen3-TTS needs ~2.7 GB beside the recorder, so it is kept for
/// machines with room to spare; Supertonic fits a 16 GB Mac; anything smaller
/// gets Kokoro, the smallest voice that still sounds natural.
pub fn auto_engine(total_ram: u64) -> Engine {
    if total_ram >= 32 * GIB {
        Engine::Qwen3Tts
    } else if total_ram >= 16 * GIB {
        Engine::Supertonic3
    } else {
        Engine::Kokoro
    }
}

/// The engine this machine runs: the setting if there is one, else by memory.
pub fn resolve_engine(setting: Option<Engine>, total_ram: u64) -> Engine {
    setting.unwrap_or_else(|| auto_engine(total_ram))
}

/// Installed physical memory, from `hw.memsize`. Zero if the kernel will not
/// say, which [`auto_engine`] reads as a small machine.
pub fn total_ram() -> u64 {
    let mut bytes: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let name = c"hw.memsize";
    // SAFETY: `name` is NUL-terminated, and `bytes`/`len` describe a u64 the
    // kernel writes at most `len` bytes into.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut bytes as *mut u64).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 {
        bytes
    } else {
        0
    }
}

/// The `voice/` folder of the source checkout this binary sits in, found by
/// walking up from the executable (`target/…/ambient` or
/// `build/Ambient.app/…`). Found at run time, not compiled in, so a release
/// binary carries no build path. Elsewhere, set `voice.helper_dir`.
pub fn default_helper_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.ancestors()
                .map(|dir| dir.join("voice"))
                .find(|dir| dir.join("pyproject.toml").is_file())
        })
        .unwrap_or_else(|| PathBuf::from("voice"))
}

/// The command that runs the helper for `engine`: `uv run` in the helper's
/// project, installing only that engine's extra. `uv` resolves the locked
/// environment on first use, which is the model runtime's one-time download.
pub fn helper_command(
    dir: &Path,
    engine: Engine,
    voice: &crate::config::VoiceConfig,
    extra: &[String],
) -> (PathBuf, Vec<String>) {
    let mut args: Vec<String> = vec![
        "run".into(),
        "--quiet".into(),
        "--frozen".into(),
        "--project".into(),
        dir.display().to_string(),
        "--extra".into(),
        engine.as_str().into(),
        "--extra".into(),
        "play".into(),
        "ambient-voice".into(),
        "--engine".into(),
        engine.as_str().into(),
    ];
    let mut flag = |name: &str, value: Option<String>| {
        if let Some(v) = value {
            args.extend([name.to_string(), v]);
        }
    };
    flag("--voice", voice.speaker.clone());
    flag("--style", voice.style.clone());
    // A designed voice is Qwen3-TTS only; other engines would refuse it.
    if engine == Engine::Qwen3Tts {
        match &voice.reference {
            // A relative reference names a voice shipped with the helper,
            // such as `voices/v1-gravel`, so the setting survives moving
            // the checkout.
            Some(r) => flag("--reference", Some(dir.join(r).display().to_string())),
            None => {
                flag("--description", voice.description.clone());
                if voice.description.is_some() {
                    flag("--cue", voice.cue.clone());
                }
            }
        }
    }
    args.extend(extra.iter().cloned());
    (PathBuf::from("uv"), args)
}

/// What a finished utterance reports.
#[derive(Debug, Clone, PartialEq)]
pub struct Spoken {
    pub first_audio_ms: Option<u64>,
    pub audio_s: f64,
}

/// One running helper process.
struct Helper {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<Value>,
    next_id: u64,
}

impl Helper {
    fn start(program: &Path, args: &[String], ready_within: Duration) -> Result<Helper> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The helper's diagnostics land in `ambient mcp`'s stderr, which
            // the MCP client keeps as the server's log. Never its stdout:
            // that is the protocol.
            .stderr(Stdio::inherit())
            // Its own process group, so `kill` reaches the real helper that
            // `uv run` starts, not just `uv`.
            .process_group(0)
            .spawn()
            .with_context(|| format!("could not start the voice helper ({})", program.display()))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                // A line that is not JSON is the helper's bug; it is skipped
                // rather than mistaken for an answer.
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
            }
            // Dropping `tx` here is how the reader says the helper is gone.
        });
        let mut helper = Helper {
            child,
            stdin,
            events: rx,
            next_id: 1,
        };
        match helper.events.recv_timeout(ready_within) {
            Ok(v) if v["event"] == "ready" => Ok(helper),
            Ok(v) => {
                helper.kill();
                bail!("the voice helper said {v} before it was ready")
            }
            Err(RecvTimeoutError::Timeout) => {
                helper.kill();
                bail!(
                    "the voice helper was not ready within {}s",
                    ready_within.as_secs()
                )
            }
            Err(RecvTimeoutError::Disconnected) => {
                let status = helper.child.wait().ok();
                bail!("the voice helper exited before it was ready ({status:?})")
            }
        }
    }

    fn say(&mut self, text: &str, emotion: &str, within: Duration) -> Result<Spoken> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"op": "say", "id": id, "text": text, "emotion": emotion});
        writeln!(self.stdin, "{request}")
            .and_then(|_| self.stdin.flush())
            .map_err(|e| anyhow!("the voice helper is gone ({e})"))?;
        let deadline = Instant::now() + within;
        let mut first_audio_ms = None;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = match self.events.recv_timeout(left) {
                Ok(v) => v,
                Err(e @ RecvTimeoutError::Timeout) => {
                    return Err(anyhow!(e).context(format!(
                        "the voice helper did not finish within {}s",
                        within.as_secs()
                    )))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.reap();
                    bail!("the voice helper exited mid-reply")
                }
            };
            // Events for an earlier request (one that timed out) are stale.
            if event["id"].as_u64() != Some(id) {
                continue;
            }
            match event["event"].as_str() {
                Some("speaking") => first_audio_ms = event["first_audio_ms"].as_u64(),
                Some("done") => {
                    return Ok(Spoken {
                        first_audio_ms,
                        audio_s: event["audio_s"].as_f64().unwrap_or(0.0),
                    })
                }
                Some("error") => bail!(
                    "the voice helper could not say it: {}",
                    event["message"].as_str().unwrap_or("no reason given")
                ),
                _ => {}
            }
        }
    }

    /// Its stdout closed, so it is on its way out: wait briefly for the exit
    /// to land, so `alive` agrees, and kill it if it lingers.
    fn reap(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if !self.alive() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.kill();
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Ask it to leave, then make sure it has.
    fn stop(mut self) {
        let _ = writeln!(self.stdin, r#"{{"op":"quit"}}"#);
        let _ = self.stdin.flush();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if !self.alive() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.kill();
    }

    fn kill(&mut self) {
        // SAFETY: plain syscall; the group id is the child's pid, set at spawn.
        unsafe { libc::killpg(self.child.id() as libc::pid_t, libc::SIGKILL) };
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        if self.alive() {
            self.kill();
        }
    }
}

/// How long to wait for each stage, and when to give up.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Loading a model from disk; the first run also downloads it.
    pub ready_within: Duration,
    /// One utterance, start to last sample.
    pub say_within: Duration,
    /// Consecutive failed starts or crashes before speech is given up.
    pub max_failures: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // The first run downloads the model, and designing a voice
            // downloads the 1.7B VoiceDesign model too.
            ready_within: Duration::from_secs(1800),
            say_within: Duration::from_secs(120),
            max_failures: 3,
        }
    }
}

/// The supervised voice: started on demand, restarted after a crash, and
/// switched off for the rest of the watch after `max_failures` failures in a
/// row.
pub struct Voice {
    program: PathBuf,
    args: Vec<String>,
    limits: Limits,
    helper: Option<Helper>,
    failures: u32,
    given_up: bool,
    pid_slot: Option<&'static AtomicI32>,
}

impl Voice {
    pub fn new(program: PathBuf, args: Vec<String>, limits: Limits) -> Self {
        Self {
            program,
            args,
            limits,
            helper: None,
            failures: 0,
            given_up: false,
            pid_slot: None,
        }
    }

    /// Keep `slot` holding the running helper's process group, or 0 when
    /// none runs, so a signal handler can silence it mid-reply: `ambient
    /// talk` stops speaking the moment the user talks over it.
    pub fn with_pid_slot(mut self, slot: &'static AtomicI32) -> Self {
        self.pid_slot = Some(slot);
        self
    }

    fn publish_pid(&self) {
        if let Some(slot) = self.pid_slot {
            let pid = self.helper.as_ref().map_or(0, |h| h.child.id() as i32);
            slot.store(pid, Ordering::SeqCst);
        }
    }

    pub fn is_running(&mut self) -> bool {
        self.helper.as_mut().is_some_and(Helper::alive)
    }

    pub fn given_up(&self) -> bool {
        self.given_up
    }

    /// Start the helper now rather than on the first reply, so the model has
    /// loaded before anyone needs it.
    pub fn warm(&mut self) -> Result<()> {
        self.ensure_started().map(|_| ())
    }

    fn ensure_started(&mut self) -> Result<&mut Helper> {
        if self.given_up {
            bail!(
                "speech is off for this run: the voice helper failed {} times",
                self.failures
            );
        }
        if !self.is_running() {
            if let Some(dead) = self.helper.take() {
                drop(dead);
            }
            match Helper::start(&self.program, &self.args, self.limits.ready_within) {
                Ok(h) => self.helper = Some(h),
                Err(e) => {
                    self.publish_pid();
                    self.failed();
                    return Err(e);
                }
            }
            self.publish_pid();
        }
        Ok(self.helper.as_mut().expect("started above"))
    }

    fn failed(&mut self) {
        self.failures += 1;
        if self.failures >= self.limits.max_failures {
            self.given_up = true;
        }
    }

    /// Say `text`. A helper that died since the last reply is restarted first;
    /// one that dies mid-reply is restarted and asked once more.
    pub fn say(&mut self, text: &str, emotion: &str) -> Result<Spoken> {
        let within = self.limits.say_within;
        let first = self.ensure_started()?.say(text, emotion, within);
        let result = match first {
            Err(e) if !timed_out(&e) && !self.is_running() => {
                eprintln!("  voice: {e:#}; restarting the helper");
                self.failed();
                self.helper = None;
                self.ensure_started()?.say(text, emotion, within)
            }
            other => other,
        };
        if result.as_ref().is_err_and(timed_out) {
            self.helper = None;
            self.publish_pid();
        }
        match &result {
            Ok(_) => self.failures = 0,
            Err(_) if !self.is_running() => self.failed(),
            Err(_) => {}
        }
        result
    }

    pub fn stop(&mut self) {
        if let Some(h) = self.helper.take() {
            h.stop();
        }
        self.publish_pid();
    }
}

/// `speaking.lock`, beside the config file: held while a voice speaks, so the
/// live assistant and `ambient talk` never talk over each other. It names the
/// holder's pid and what it is (`"assistant"` or `"talk"`); a lock whose pid
/// is gone is a crash's leftover and is taken over.
pub struct SpeakingLock {
    path: PathBuf,
    mine: String,
}

/// Who holds `speaking.lock`, read from it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Holder {
    pub pid: u32,
    pub who: String,
}

impl SpeakingLock {
    /// The lock file for this config file.
    pub fn path(config_file: &Path) -> PathBuf {
        config_file.with_file_name("speaking.lock")
    }

    /// The live holder, if any: a lock naming a pid that is gone holds
    /// nothing.
    pub fn holder(path: &Path) -> Option<Holder> {
        let h: Holder = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        pid_alive(h.pid).then_some(h)
    }

    /// Take the lock as `who`, waiting up to `wait` for another voice to
    /// finish.
    pub fn acquire(path: &Path, who: &str, wait: Duration) -> Result<SpeakingLock> {
        let mine = json!({"pid": std::process::id(), "who": who}).to_string();
        let deadline = Instant::now() + wait;
        loop {
            let created = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path);
            match created {
                Ok(mut f) => {
                    f.write_all(mine.as_bytes())?;
                    return Ok(SpeakingLock {
                        path: path.to_path_buf(),
                        mine,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    return Err(anyhow!(e).context(format!("could not take {}", path.display())))
                }
            }
            let held = std::fs::read_to_string(path).unwrap_or_default();
            let live = serde_json::from_str::<Holder>(&held)
                .ok()
                .filter(|h| pid_alive(h.pid));
            match live {
                // Only the leftover just read is removed, so a lock another
                // process took over meanwhile survives.
                None => {
                    if std::fs::read_to_string(path).unwrap_or_default() == held {
                        let _ = std::fs::remove_file(path);
                    }
                }
                Some(h) if Instant::now() >= deadline => {
                    bail!("the {} (pid {}) is speaking", h.who, h.pid)
                }
                Some(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

impl Drop for SpeakingLock {
    fn drop(&mut self) {
        if std::fs::read_to_string(&self.path).is_ok_and(|t| t == self.mine) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 sends nothing; it only asks whether `pid` exists.
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

/// A say that ran out of time, which leaves the helper wedged.
fn timed_out(e: &anyhow::Error) -> bool {
    e.root_cause().is::<RecvTimeoutError>()
}

impl Drop for Voice {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_picks_by_installed_memory() {
        assert_eq!(auto_engine(128 * GIB), Engine::Qwen3Tts);
        assert_eq!(auto_engine(32 * GIB), Engine::Qwen3Tts);
        assert_eq!(auto_engine(24 * GIB), Engine::Supertonic3);
        assert_eq!(auto_engine(16 * GIB), Engine::Supertonic3);
        assert_eq!(auto_engine(8 * GIB), Engine::Kokoro);
        assert_eq!(
            auto_engine(0),
            Engine::Kokoro,
            "unknown memory is a small machine"
        );
    }

    #[test]
    fn a_configured_engine_wins_over_memory() {
        assert_eq!(
            resolve_engine(Some(Engine::Kokoro), 128 * GIB),
            Engine::Kokoro
        );
        assert_eq!(
            resolve_engine(Some(Engine::Qwen3Tts), 8 * GIB),
            Engine::Qwen3Tts
        );
        assert_eq!(resolve_engine(None, 16 * GIB), Engine::Supertonic3);
    }

    #[test]
    fn this_mac_reports_its_memory() {
        assert!(total_ram() >= 8 * GIB, "{}", total_ram());
    }

    #[test]
    fn engine_names_round_trip_through_settings_and_json() {
        for e in [Engine::Qwen3Tts, Engine::Supertonic3, Engine::Kokoro] {
            assert_eq!(e.as_str().parse::<Engine>().unwrap(), e);
            assert_eq!(serde_json::to_value(e).unwrap(), json!(e.as_str()));
        }
        assert!("espeak".parse::<Engine>().is_err());
    }

    #[test]
    fn the_command_installs_only_the_chosen_engine() {
        let voice = crate::config::VoiceConfig {
            speaker: Some("F2".into()),
            description: Some("An older Scottish man.".into()),
            ..Default::default()
        };
        let (program, args) = helper_command(
            Path::new("/v"),
            Engine::Supertonic3,
            &voice,
            &["--no-play".into()],
        );
        assert_eq!(program, PathBuf::from("uv"));
        let joined = args.join(" ");
        assert!(
            joined.contains("--extra supertonic3 --extra play"),
            "{joined}"
        );
        assert!(!joined.contains("qwen3-tts"), "{joined}");
        assert!(
            joined.ends_with("--engine supertonic3 --voice F2 --no-play"),
            "{joined}"
        );
    }

    #[test]
    fn a_designed_voice_reaches_qwen_and_a_reference_wins_over_a_description() {
        let mut voice = crate::config::VoiceConfig {
            description: Some("An older Scottish man.".into()),
            cue: Some("Deadpan.".into()),
            ..Default::default()
        };
        let (_, args) = helper_command(Path::new("/v"), Engine::Qwen3Tts, &voice, &[]);
        let joined = args.join(" ");
        assert!(
            joined.ends_with("--description An older Scottish man. --cue Deadpan."),
            "{joined}"
        );
        voice.reference = Some(PathBuf::from("/voices/gravel"));
        let (_, args) = helper_command(Path::new("/v"), Engine::Qwen3Tts, &voice, &[]);
        let joined = args.join(" ");
        assert!(joined.ends_with("--reference /voices/gravel"), "{joined}");
        assert!(!joined.contains("--description"), "{joined}");
        voice.reference = Some(PathBuf::from("voices/v1-gravel"));
        let (_, args) = helper_command(Path::new("/v"), Engine::Qwen3Tts, &voice, &[]);
        assert!(
            args.join(" ").ends_with("--reference /v/voices/v1-gravel"),
            "{args:?}"
        );
        let (_, args) = helper_command(Path::new("/v"), Engine::Kokoro, &voice, &[]);
        assert!(
            !args.join(" ").contains("--reference"),
            "only Qwen3-TTS clones"
        );
    }

    /// A stand-in helper written as a shell script, so these tests exercise
    /// real processes, pipes and exits without a model. `mode` picks how it
    /// misbehaves; `marker` lets it remember across restarts.
    fn fake(name: &str, body: &str) -> (PathBuf, Vec<String>) {
        let dir = std::env::temp_dir().join(format!("ambient-voice-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("helper.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nM={}\n{body}", dir.join("marker").display()),
        )
        .unwrap();
        (PathBuf::from("/bin/sh"), vec![script.display().to_string()])
    }

    /// Answers each `say` with speaking + done, echoing the request's id.
    const SPEAKS: &str = r#"
echo '{"event":"ready","engine":"fake"}'
while read -r line; do
  case "$line" in *quit*) exit 0;; esac
  id=$(echo "$line" | sed 's/.*"id":\([0-9]*\).*/\1/')
  echo "{\"event\":\"speaking\",\"id\":$id,\"first_audio_ms\":90}"
  echo "{\"event\":\"done\",\"id\":$id,\"audio_s\":1.5}"
done
"#;

    fn quick() -> Limits {
        Limits {
            ready_within: Duration::from_secs(5),
            say_within: Duration::from_secs(5),
            max_failures: 3,
        }
    }

    #[test]
    fn it_starts_on_the_first_reply_and_reports_what_was_said() {
        let (p, a) = fake("speaks", SPEAKS);
        let mut v = Voice::new(p, a, quick());
        assert!(!v.is_running(), "nothing starts until it is needed");
        let got = v.say("hello", "warm").unwrap();
        assert_eq!(
            got,
            Spoken {
                first_audio_ms: Some(90),
                audio_s: 1.5
            }
        );
        assert!(v.is_running());
        assert_eq!(v.say("again", "neutral").unwrap().audio_s, 1.5);
        v.stop();
        assert!(!v.is_running());
    }

    #[test]
    fn a_helper_that_crashes_mid_reply_is_restarted_and_asked_again() {
        // The first run reads one request and dies; the restart behaves.
        let body = format!(
            r#"if [ ! -e "$M" ]; then
  touch "$M"; echo '{{"event":"ready"}}'; read -r line; exit 3
fi
{SPEAKS}"#
        );
        let (p, a) = fake("crash-once", &body);
        let mut v = Voice::new(p, a, quick());
        assert_eq!(v.say("hello", "warm").unwrap().audio_s, 1.5);
        assert!(!v.given_up());
    }

    #[test]
    fn a_helper_that_keeps_dying_turns_speech_off_rather_than_the_assistant() {
        let (p, a) = fake(
            "always-dies",
            r#"echo '{"event":"ready"}'; read -r line; exit 3"#,
        );
        let mut v = Voice::new(p, a, quick());
        assert!(v.say("one", "warm").is_err());
        assert!(v.say("two", "warm").is_err());
        assert!(v.given_up(), "three failures in a row");
        let e = v.say("three", "warm").unwrap_err();
        assert!(e.to_string().contains("speech is off"), "{e}");
    }

    #[test]
    fn a_helper_that_never_answers_is_replaced_and_given_up_on_after_repeated_hangs() {
        let body = r#"echo started >> "$M"
echo '{"event":"ready"}'
while read -r line; do :; done
"#;
        let (p, a) = fake("hangs", body);
        let marker = PathBuf::from(&a[0]).with_file_name("marker");
        let mut v = Voice::new(
            p,
            a,
            Limits {
                say_within: Duration::from_millis(300),
                ..quick()
            },
        );
        let started = Instant::now();
        let e = v.say("one", "warm").unwrap_err();
        assert!(e.to_string().contains("did not finish"), "{e}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "not asked twice"
        );
        assert!(!v.is_running(), "the wedged helper is gone");
        v.say("two", "warm").unwrap_err();
        let starts = std::fs::read_to_string(&marker).unwrap().lines().count();
        assert_eq!(starts, 2, "the next reply got a fresh helper");
        v.say("three", "warm").unwrap_err();
        assert!(v.given_up(), "three hangs in a row");
    }

    #[test]
    fn a_restarted_helper_that_hangs_is_replaced_and_counted() {
        let body = r#"if [ ! -e "$M" ]; then
  touch "$M"; echo '{"event":"ready"}'; read -r line; exit 3
fi
echo started >> "$M.hung"
echo '{"event":"ready"}'
while read -r line; do :; done
"#;
        let (p, a) = fake("crash-then-hang", body);
        let hung = PathBuf::from(&a[0]).with_file_name("marker.hung");
        let mut v = Voice::new(
            p,
            a,
            Limits {
                say_within: Duration::from_millis(300),
                ..quick()
            },
        );
        let e = v.say("one", "warm").unwrap_err();
        assert!(e.to_string().contains("did not finish"), "{e}");
        assert!(!v.is_running(), "the hung replacement is gone");
        v.say("two", "warm").unwrap_err();
        let starts = std::fs::read_to_string(&hung).unwrap().lines().count();
        assert_eq!(starts, 2, "the next reply got a fresh helper");
        assert!(v.given_up(), "a crash and two hangs in a row");
    }

    #[test]
    fn killing_a_hung_helper_also_kills_the_process_it_started() {
        // Like `uv run`: the started process runs the real helper as its
        // own child and waits for it, so killing only the parent orphans it.
        // The real helper is wedged, so it never notices stdin closing.
        let body = r#"/bin/sh -c 'echo $$ > "$0"; echo "{\"event\":\"ready\"}"; exec sleep 30' "$M.pid"
echo "the helper exited" >&2
"#;
        let (p, a) = fake("grandchild", body);
        let pid_file = PathBuf::from(&a[0]).with_file_name("marker.pid");
        let mut v = Voice::new(
            p,
            a,
            Limits {
                say_within: Duration::from_millis(300),
                ..quick()
            },
        );
        v.say("one", "warm").unwrap_err();
        assert!(!v.is_running());
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        // SAFETY: signal 0 only checks whether the process exists.
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let survived = unsafe { libc::kill(pid, 0) } == 0;
        if survived {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        assert!(!survived, "the real helper (pid {pid}) outlived the kill");
    }

    #[test]
    fn the_pid_slot_names_the_running_helper_and_clears_when_it_stops() {
        static SLOT: AtomicI32 = AtomicI32::new(-1);
        let (p, a) = fake("slot", SPEAKS);
        let mut v = Voice::new(p, a, quick()).with_pid_slot(&SLOT);
        v.warm().unwrap();
        let pid = SLOT.load(Ordering::SeqCst);
        assert!(pid > 0);
        assert_eq!(v.helper.as_ref().unwrap().child.id() as i32, pid);
        v.stop();
        assert_eq!(SLOT.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_helper_that_never_gets_ready_is_killed_at_the_deadline() {
        let (p, a) = fake("never-ready", "sleep 30");
        let mut v = Voice::new(
            p,
            a,
            Limits {
                ready_within: Duration::from_millis(300),
                ..quick()
            },
        );
        let started = Instant::now();
        let e = v.say("hello", "warm").unwrap_err();
        assert!(e.to_string().contains("not ready"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!v.is_running());
    }

    #[test]
    fn a_helper_that_cannot_load_its_model_says_so() {
        let (p, a) = fake("no-model", "echo 'cannot load' >&2; exit 2");
        let mut v = Voice::new(p, a, quick());
        let e = v.say("hello", "warm").unwrap_err();
        assert!(e.to_string().contains("exited before it was ready"), "{e}");
    }

    #[test]
    fn an_engine_error_is_reported_without_restarting_a_healthy_helper() {
        let body = r#"
echo '{"event":"ready"}'
while read -r line; do
  id=$(echo "$line" | sed 's/.*"id":\([0-9]*\).*/\1/')
  echo "{\"event\":\"error\",\"id\":$id,\"message\":\"too long\"}"
done
"#;
        let (p, a) = fake("engine-error", body);
        let mut v = Voice::new(p, a, quick());
        let e = v.say("hello", "warm").unwrap_err();
        assert!(e.to_string().contains("too long"), "{e}");
        assert!(v.is_running(), "the helper is fine; only that line failed");
        assert!(!v.given_up());
    }

    #[test]
    fn a_stopped_helper_comes_back_when_needed() {
        let (p, a) = fake("idle", SPEAKS);
        let mut v = Voice::new(p, a, quick());
        v.say("hello", "warm").unwrap();
        v.stop();
        assert!(!v.is_running());
        v.say("back", "warm").unwrap();
        assert!(v.is_running());
    }

    #[test]
    fn stop_kills_a_helper_that_ignores_quit() {
        let body = r#"trap '' TERM
echo '{"event":"ready"}'
while true; do sleep 1; done
"#;
        let (p, a) = fake("stubborn", body);
        let mut v = Voice::new(p, a, quick());
        v.warm().unwrap();
        let started = Instant::now();
        v.stop();
        assert!(!v.is_running());
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
