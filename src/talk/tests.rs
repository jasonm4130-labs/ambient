use super::*;
use crate::assist::voice::{SpeakingLock, Spoken};
use std::cell::RefCell;
use std::rc::Rc;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ambient-talk-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn markdown_is_read_as_plain_sentences() {
    let md = "## Status\n\n- **PR 12** merged\n- see [the board](https://x.y/z) and `fm-inbox.sh`\n\n\
              ```sh\nrm -rf /\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nDetails at https://example.com/a.";
    assert_eq!(
        speakable(md),
        "Status. PR 12 merged. see the board and fm-inbox.sh. a b. 1 2. Details at a link"
    );
    assert_eq!(speakable("1. one\n2) two!"), "one. two!");
    assert_eq!(speakable("> _quoted_ ~~text~~"), "quoted text");
    assert_eq!(speakable("```\nonly code\n```"), "");
}

#[test]
fn a_long_reply_is_cut_at_a_sentence_or_a_word() {
    let short = "Done.";
    assert_eq!(cap(short, 600), short);
    let sentences = "This is a sentence. ".repeat(40);
    let c = cap(&sentences, 600);
    assert!(c.chars().count() <= 600, "{}", c.len());
    assert!(c.ends_with('.'), "{c}");
    let words = "word ".repeat(200);
    let c = cap(&words, 600);
    assert!(c.chars().count() <= 600);
    assert!(c.ends_with("word…"), "{c}");
    let wide = "é".repeat(700);
    assert!(cap(&wide, 600).chars().count() <= 600);
}

#[derive(Default)]
struct FakeTarget {
    ready: Option<bool>,
    sent: Vec<(String, String)>,
    /// Polls before the reply shows up, then the reply.
    polls_before: usize,
    reply: Option<String>,
    polled: usize,
    /// Called with each poll's number, to change the world while waiting.
    on_poll: Option<Box<dyn FnMut(usize)>>,
}

impl Target for FakeTarget {
    fn ready(&mut self) -> Result<Option<bool>> {
        Ok(self.ready)
    }
    fn send(&mut self, text: &str, request_id: &str) -> Result<Sent> {
        self.sent.push((text.into(), request_id.into()));
        Ok(Sent {
            id: "n1".into(),
            announced: true,
        })
    }
    fn reply(&mut self, id: &str) -> Result<Option<String>> {
        assert_eq!(id, "n1");
        self.polled += 1;
        if let Some(f) = self.on_poll.as_mut() {
            f(self.polled);
        }
        Ok((self.polled > self.polls_before)
            .then(|| self.reply.clone())
            .flatten())
    }
}

#[derive(Default, Clone)]
struct FakeVoice {
    said: Rc<RefCell<Vec<String>>>,
    warmed: Rc<RefCell<bool>>,
    fail: bool,
}

impl Speaker for FakeVoice {
    fn say(&mut self, text: &str, _emotion: &str) -> Result<Spoken> {
        if self.fail {
            bail!("no voice");
        }
        self.said.borrow_mut().push(text.into());
        Ok(Spoken {
            first_audio_ms: Some(50),
            audio_s: 1.0,
        })
    }
    fn warm(&mut self) -> Result<()> {
        *self.warmed.borrow_mut() = true;
        Ok(())
    }
}

fn turn<'a>(
    target: &'a mut FakeTarget,
    speaker: Option<&'a mut dyn Speaker>,
    lock: &Path,
    paused: &'a dyn Fn() -> bool,
    out: &'a mut Vec<u8>,
) -> Turn<'a> {
    Turn {
        target,
        speaker,
        lock: lock.to_path_buf(),
        paused,
        poll: Duration::from_millis(1),
        timeout: Duration::from_secs(5),
        out,
    }
}

fn events(out: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(out)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn a_reply_to_the_note_is_spoken_once_it_arrives() {
    let dir = temp("spoken");
    let mut target = FakeTarget {
        ready: Some(true),
        polls_before: 3,
        reply: Some("**Yes.** It merged.".into()),
        ..Default::default()
    };
    let mut voice = FakeVoice::default();
    let mut out = Vec::new();
    let no = || false;
    let lock = dir.join("speaking.lock");
    let got = converse(
        "did it merge",
        turn(&mut target, Some(&mut voice), &lock, &no, &mut out),
    )
    .unwrap();
    assert_eq!(got, Outcome::Spoken);
    assert_eq!(target.sent[0].0, "did it merge");
    assert!(
        target.sent[0].1.starts_with("ambient-"),
        "{:?}",
        target.sent
    );
    assert_eq!(*voice.said.borrow(), ["Yes. It merged."]);
    assert!(
        *voice.warmed.borrow(),
        "the voice warms while the agent thinks"
    );
    assert!(!lock.exists(), "the speaking lock is released");
    let kinds: Vec<_> = events(&out)
        .iter()
        .map(|e| e["event"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds, ["sent", "reply", "spoken"]);
}

#[test]
fn a_dry_run_prints_what_it_would_say() {
    let dir = temp("dry");
    let mut target = FakeTarget {
        ready: Some(false),
        reply: Some("Fine.".into()),
        ..Default::default()
    };
    let mut out = Vec::new();
    let no = || false;
    let got = converse("hi", turn(&mut target, None, &dir.join("l"), &no, &mut out)).unwrap();
    assert_eq!(got, Outcome::DryRun);
    let ev = events(&out);
    assert_eq!(
        ev[0]["event"], "queued",
        "a question nobody is listening for still waits"
    );
    assert_eq!(ev.last().unwrap()["would_say"], "Fine.");
}

#[test]
fn no_reply_within_the_timeout_is_said_so() {
    let dir = temp("timeout");
    let mut target = FakeTarget {
        ready: Some(true),
        ..Default::default()
    };
    let mut out = Vec::new();
    let no = || false;
    let mut t = turn(&mut target, None, &dir.join("l"), &no, &mut out);
    t.timeout = Duration::from_millis(20);
    assert_eq!(converse("hi", t).unwrap(), Outcome::NoReply);
}

/// D3: a call arming while the turn waits, then going away without a
/// recording, leaves the reply to be said as usual; one still armed when the
/// reply comes is shown, not said.
#[test]
fn a_reply_is_said_after_a_call_arms_and_disarms_but_not_while_armed() {
    let dir = temp("armed");
    let lock = dir.join("speaking.lock");
    let root = dir.join("sessions");
    std::fs::create_dir_all(&root).unwrap();
    let paused = || is_paused_in(&root);
    let target = |disarm: bool| {
        let root = root.clone();
        FakeTarget {
            ready: Some(true),
            polls_before: 3,
            reply: Some("Merged.".into()),
            on_poll: Some(Box::new(move |n| match n {
                1 => set_paused(&root, true).unwrap(),
                2 if disarm => set_paused(&root, false).unwrap(),
                _ => {}
            })),
            ..Default::default()
        }
    };

    let mut voice = FakeVoice::default();
    let (mut t, mut out) = (target(true), Vec::new());
    let got = converse(
        "hi",
        turn(&mut t, Some(&mut voice), &lock, &paused, &mut out),
    )
    .unwrap();
    assert_eq!(got, Outcome::Spoken);
    assert_eq!(*voice.said.borrow(), ["Merged."]);

    let mut voice = FakeVoice::default();
    let (mut t, mut out) = (target(false), Vec::new());
    let got = converse(
        "hi",
        turn(&mut t, Some(&mut voice), &lock, &paused, &mut out),
    )
    .unwrap();
    assert!(matches!(got, Outcome::TextOnly(_)), "{got:?}");
    assert!(voice.said.borrow().is_empty());
    let kinds: Vec<_> = events(&out)
        .iter()
        .map(|e| e["event"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds, ["sent", "reply", "text_only"]);
    set_paused(&root, false).unwrap();
    assert!(!is_paused_in(&root));
    set_paused(&root, false).unwrap();
}

/// A fresh install has no sessions folder until its first recording; a call
/// arming before then must still keep a reply from being said.
#[test]
fn pausing_works_before_the_sessions_folder_exists() {
    let root = temp("no-sessions").join("not-yet");
    assert!(!root.exists());
    set_paused(&root, false).unwrap();
    assert!(!is_paused_in(&root));
    set_paused(&root, true).unwrap();
    assert!(is_paused_in(&root));
}

#[test]
fn the_reply_stays_text_when_the_voice_fails_or_a_recording_starts() {
    let dir = temp("fallback");
    let lock = dir.join("speaking.lock");
    let reply = || FakeTarget {
        ready: Some(true),
        reply: Some("Hello.".into()),
        ..Default::default()
    };
    let no = || false;
    let yes = || true;

    let mut broken = FakeVoice {
        fail: true,
        ..Default::default()
    };
    let (mut t, mut out) = (reply(), Vec::new());
    let got = converse("hi", turn(&mut t, Some(&mut broken), &lock, &no, &mut out)).unwrap();
    assert!(
        matches!(got, Outcome::TextOnly(ref w) if w.contains("no voice")),
        "{got:?}"
    );
    assert_eq!(events(&out).last().unwrap()["event"], "text_only");

    let mut voice = FakeVoice::default();
    let (mut t, mut out) = (reply(), Vec::new());
    t.polls_before = 3;
    let got = converse("hi", turn(&mut t, Some(&mut voice), &lock, &yes, &mut out)).unwrap();
    assert!(
        matches!(got, Outcome::TextOnly(ref w) if w.contains("recording")),
        "{got:?}"
    );
    assert!(voice.said.borrow().is_empty());
    let kinds: Vec<_> = events(&out)
        .iter()
        .map(|e| e["event"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        kinds,
        ["sent", "reply", "text_only"],
        "the reply is still shown"
    );
    assert!(!lock.exists(), "the speaking lock is released");
}

#[test]
fn a_held_speaking_lock_is_waited_for_and_a_dead_holders_is_taken_over() {
    let dir = temp("lock");
    let path = dir.join("speaking.lock");
    let held = SpeakingLock::acquire(&path, "assistant", Duration::ZERO).unwrap();
    let h = SpeakingLock::holder(&path).unwrap();
    assert_eq!((h.pid, h.who.as_str()), (std::process::id(), "assistant"));
    let e = SpeakingLock::acquire(&path, "talk", Duration::from_millis(100))
        .err()
        .unwrap();
    assert!(e.to_string().contains("assistant"), "{e}");
    drop(held);
    assert!(!path.exists());
    // A crash leaves the file behind naming a pid that is gone.
    std::fs::write(&path, r#"{"pid":999999,"who":"talk"}"#).unwrap();
    assert_eq!(SpeakingLock::holder(&path), None);
    let _l = SpeakingLock::acquire(&path, "talk", Duration::ZERO).unwrap();
}

#[test]
fn a_staged_lock_left_by_a_dead_process_is_swept() {
    let dir = temp("lock-sweep");
    let path = dir.join("speaking.lock");
    let dead = dir.join("speaking.999999-0.tmp");
    let live = dir.join(format!("speaking.{}-999.tmp", std::process::id()));
    let other = dir.join("notes.999999-0.tmp");
    for f in [&dead, &live, &other] {
        std::fs::write(f, "{}").unwrap();
    }
    drop(SpeakingLock::acquire(&path, "talk", Duration::ZERO).unwrap());
    assert!(!dead.exists(), "a dead process's staged lock is removed");
    assert!(live.exists(), "a live process's staged lock is kept");
    assert!(other.exists(), "an unrelated file is kept");
}

#[test]
fn a_lock_caught_mid_write_is_held_until_it_is_stale() {
    let dir = temp("lock-empty");
    let path = dir.join("speaking.lock");
    std::fs::write(&path, "").unwrap();
    assert!(SpeakingLock::acquire(&path, "talk", Duration::from_millis(100)).is_err());
    assert!(path.exists(), "a fresh unreadable lock is not removed");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(60))
        .unwrap();
    let l = SpeakingLock::acquire(&path, "talk", Duration::ZERO).unwrap();
    assert_eq!(SpeakingLock::holder(&path).unwrap().who, "talk");
    drop(l);
    let left: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert!(left.is_empty(), "nothing staged is left behind: {left:?}");
}

/// A config file in a fresh folder, so the mailbox beside it is this test's.
fn config_in(name: &str) -> PathBuf {
    temp(name).join("config.json")
}

fn listening_here(config_file: &Path) {
    mailbox::write_beat(
        config_file,
        &mailbox::Listener {
            pid: std::process::id(),
            id: "test".into(),
            since: chrono::Local::now().to_rfc3339(),
            push: true,
            client: Some("test".into()),
        },
    )
    .unwrap();
}

/// The whole headless round trip through the real mailbox: a thread stands
/// in for the agent and answers once the question shows up, and the answer
/// is said, in plain words, and then nothing is left on disk.
#[test]
fn the_mailbox_target_delivers_and_speaks_the_agents_reply() {
    let cfg = config_in("mb-spoken");
    listening_here(&cfg);
    let mut target = MailboxTarget::new(&cfg, Duration::from_secs(5)).unwrap();
    let agent_cfg = cfg.clone();
    let agent = std::thread::spawn(move || {
        let mb = mailbox::Mailbox::at(&agent_cfg).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(n) = mb.open_notes().pop() {
                assert_eq!(n.text, "what is the build status");
                mb.reply(
                    &n.request_id,
                    "**Green.** All tests pass.\n\n- 512 passed\n- 0 failed",
                )
                .unwrap();
                return n.request_id;
            }
            assert!(Instant::now() < deadline, "the question never showed up");
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let mut voice = FakeVoice::default();
    let mut out = Vec::new();
    let no = || false;
    let lock = cfg.with_file_name("speaking.lock");
    let got = converse(
        "what is the build status",
        Turn {
            target: &mut target,
            speaker: Some(&mut voice),
            lock,
            paused: &no,
            poll: Duration::from_millis(5),
            timeout: Duration::from_secs(5),
            out: &mut out,
        },
    )
    .unwrap();
    let rid = agent.join().unwrap();
    assert_eq!(got, Outcome::Spoken);
    assert_eq!(
        *voice.said.borrow(),
        ["Green. All tests pass."],
        "only the lead is said; the detail after the blank line stays text"
    );
    let ev = events(&out);
    assert_eq!(ev[0]["event"], "sent");
    assert_eq!(ev[0]["id"], rid);
    assert_eq!(ev[0]["announced"], true);
    assert_eq!(
        ev[1]["text"],
        "**Green.** All tests pass.\n\n- 512 passed\n- 0 failed"
    );
    let left: Vec<_> = std::fs::read_dir(cfg.with_file_name(mailbox::DIR))
        .unwrap()
        .collect();
    assert!(
        left.is_empty(),
        "the question and answer are cleared: {left:?}"
    );
}

#[test]
fn no_listener_is_reported_as_queued_with_the_prompt_to_run() {
    let cfg = config_in("mb-queued");
    let mut target = MailboxTarget::new(&cfg, Duration::from_millis(30)).unwrap();
    assert_eq!(target.ready().unwrap(), Some(false));
    let mut out = Vec::new();
    let no = || false;
    let got = converse(
        "anyone there",
        Turn {
            target: &mut target,
            speaker: None,
            lock: cfg.with_file_name("speaking.lock"),
            paused: &no,
            poll: Duration::from_millis(5),
            timeout: Duration::from_millis(30),
            out: &mut out,
        },
    )
    .unwrap();
    assert_eq!(got, Outcome::NoReply);
    let ev = events(&out);
    assert_eq!(ev[0]["event"], "queued");
    assert!(
        ev[0]["reason"]
            .as_str()
            .unwrap()
            .contains("/mcp__ambient__talk"),
        "{}",
        ev[0]
    );
    assert_eq!(ev[1]["announced"], false);
}

/// A turn that gives up marks its question expired, drops the text, and a
/// late answer is refused with that reason.
#[test]
fn a_timeout_expires_the_note() {
    let cfg = config_in("mb-expire");
    listening_here(&cfg);
    let mut target = MailboxTarget::new(&cfg, Duration::from_millis(30)).unwrap();
    let mut out = Vec::new();
    let no = || false;
    let got = converse(
        "hello?",
        Turn {
            target: &mut target,
            speaker: None,
            lock: cfg.with_file_name("speaking.lock"),
            paused: &no,
            poll: Duration::from_millis(5),
            timeout: Duration::from_millis(30),
            out: &mut out,
        },
    )
    .unwrap();
    assert_eq!(got, Outcome::NoReply);
    let rid = events(&out)[0]["request_id"].as_str().unwrap().to_string();
    let mb = mailbox::Mailbox::at(&cfg).unwrap();
    assert_eq!(mb.note(&rid), None, "the question's text is gone");
    assert_eq!(
        mb.reply(&rid, "Too late."),
        Err(mailbox::ReplyRefused::Expired)
    );
}

#[test]
fn only_the_lead_paragraph_is_said() {
    assert_eq!(lead("Green.\n\nDetail here."), "Green.");
    assert_eq!(lead("## Status\n\nAll green.\n\nMore."), "All green.");
    assert_eq!(
        lead("# Title\nLine one\nline two\n\nrest"),
        "Line one\nline two"
    );
    assert_eq!(lead("```sh\nls\n```\nDone.\n"), "Done.");
    assert_eq!(lead("Run this:\n```\nls\n```\nthen go"), "Run this:");
    assert_eq!(lead("## Only a heading"), "## Only a heading");
    assert_eq!(
        cap(&speakable(&lead("**Yes.** It merged.\n\n| a | b |")), 600),
        "Yes. It merged."
    );
}

#[test]
fn readable_keeps_lines_and_drops_markdown() {
    let md = "## Build\n\n**Green.** All tests pass, see [the run](https://ci.example/1).\n\n\
              - 512 passed\n* 0 `failed`\n1. first\n\n---\n\n```sh\ncargo test  --all\n```\n\
              Bare https://example.com/x stays.";
    assert_eq!(
        readable(md),
        "Build\n\nGreen. All tests pass, see the run.\n\n• 512 passed\n• 0 failed\n1. first\n\n\
         cargo test  --all\nBare https://example.com/x stays."
    );
    assert_eq!(readable("> _quoted_\n\n\n\nnext"), "quoted\n\nnext");
    assert_eq!(readable(""), "");
}

#[test]
fn pcm_is_read_as_little_endian_floats() {
    let bytes: Vec<u8> = [0.5f32, -0.25]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    assert_eq!(read_pcm(&bytes[..]).unwrap(), [0.5, -0.25]);
    assert!(read_pcm(&bytes[..3]).is_err());
}

#[test]
fn request_ids_are_fresh() {
    let (a, b) = (request_id(), request_id());
    assert_ne!(a, b);
    assert!(a.len() <= 128);
    assert!(a
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._:-".contains(c)));
}
