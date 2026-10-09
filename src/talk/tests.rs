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
    recording: &'a dyn Fn() -> bool,
    out: &'a mut Vec<u8>,
) -> Turn<'a> {
    Turn {
        target,
        speaker,
        lock: lock.to_path_buf(),
        recording,
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
        "the voice warms while firstmate thinks"
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
        "a busy firstmate still gets the note"
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
    let got = converse("hi", turn(&mut t, Some(&mut voice), &lock, &yes, &mut out)).unwrap();
    assert!(
        matches!(got, Outcome::TextOnly(ref w) if w.contains("recording")),
        "{got:?}"
    );
    assert!(voice.said.borrow().is_empty());
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

/// A stand-in `fm-inbox.sh` that keeps its state in files: `note` records
/// the body and exits 3 (saved, not announced) the first time, `announce`
/// marks it, and `receipts` shows a reply once `reply` exists, plus an older
/// reply to another note that must not be taken for this one.
const FAKE_INBOX: &str = r#"#!/bin/sh
S="$FM_HOME/state"; mkdir -p "$S"
case "$1" in
  ready) echo '{"schema":"fm-primary-ready.v1","can_receive":false}' ;;
  note)
    [ "$2" = --request-id ] || exit 1
    echo "$3" > "$S/request"; cat > "$S/body"
    echo '{"schema":"fm-inbox-note.v1","outcome":"created","id":"77-abc","announced":false,"saved":true}'
    exit 3 ;;
  announce) touch "$S/announced"; echo '{"id":"77-abc","announced":true}' ;;
  receipts)
    echo "$*" >> "$S/receipts-args"
    if [ -e "$S/reply" ] && [ "$3" = 000000000001 ]; then
      echo '{"replies":[{"id":"77-abc","body":"It is done.","cursor":"000000000002"}],"reply_cursor":"000000000002"}'
    else
      echo '{"replies":[],"reply_cursor":"000000000001"}'
    fi ;;
  *) exit 1 ;;
esac
"#;

fn fake_home(name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let home = temp(name);
    std::fs::create_dir_all(home.join("bin")).unwrap();
    let script = home.join("bin").join("fm-inbox.sh");
    std::fs::write(&script, FAKE_INBOX).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    home
}

#[test]
fn firstmate_gets_the_note_on_stdin_and_its_wake_repaired() {
    let home = fake_home("fm-send");
    let mut fm = Firstmate::new(&home).unwrap();
    assert_eq!(fm.ready().unwrap(), Some(false));
    let sent = fm.send("what's\nnext", "ambient-1").unwrap();
    assert_eq!(
        sent,
        Sent {
            id: "77-abc".into(),
            announced: true
        }
    );
    let state = home.join("state");
    assert_eq!(
        std::fs::read_to_string(state.join("body")).unwrap(),
        "what's\nnext"
    );
    assert_eq!(
        std::fs::read_to_string(state.join("request"))
            .unwrap()
            .trim(),
        "ambient-1"
    );
    assert!(
        state.join("announced").exists(),
        "exit 3 is repaired by announce"
    );
}

#[test]
fn firstmate_replies_are_read_after_the_note_and_matched_by_id() {
    let home = fake_home("fm-reply");
    let mut fm = Firstmate::new(&home).unwrap();
    fm.send("hi", "ambient-2").unwrap();
    assert_eq!(fm.reply("77-abc").unwrap(), None);
    std::fs::write(home.join("state").join("reply"), "").unwrap();
    assert_eq!(fm.reply("77-abc").unwrap().as_deref(), Some("It is done."));
    let args = std::fs::read_to_string(home.join("state").join("receipts-args")).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "receipts",
            "receipts --after 000000000001",
            "receipts --after 000000000001"
        ],
        "the cursor is taken before sending and carried forward"
    );
}

#[test]
fn a_folder_without_fm_inbox_is_not_a_firstmate_home() {
    let e = Firstmate::new(&temp("not-fm")).err().unwrap();
    assert!(e.to_string().contains("fm-inbox.sh"), "{e}");
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
fn request_ids_are_fresh_and_valid_for_fm_inbox() {
    let (a, b) = (request_id(), request_id());
    assert_ne!(a, b);
    assert!(a.len() <= 128);
    assert!(a
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._:-".contains(c)));
}
