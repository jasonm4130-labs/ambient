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

/// A stand-in `fm-inbox.sh` that keeps its state in files: `note` records
/// the body and exits 3 (saved, not announced) the first time, `announce`
/// marks it, and `receipts` pages two older replies to other notes, then
/// this note's reply once `reply` exists, one at a time unless asked for
/// all, oldest first, as firstmate's bounded receipts do.
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
    shift; after=0; all=
    while [ $# -gt 0 ]; do
      case "$1" in
        --after) after=$(expr "$2" + 0); shift 2 ;;
        --all-replies) all=1; shift ;;
        *) exit 1 ;;
      esac
    done
    rows="1 old-1 Earlier.
2 old-2 Earlier again."
    [ -e "$S/reply" ] && rows="$rows
3 77-abc It is done."
    out=; last=$after
    while read -r c id body; do
      [ "$c" -gt "$after" ] || continue
      [ -n "$out" ] && { [ -n "$all" ] || break; out="$out,"; }
      out="$out{\"id\":\"$id\",\"body\":\"$body\",\"cursor\":\"$(printf %012d "$c")\"}"
      last=$c
    done <<ROWS
$rows
ROWS
    cursor=; [ "$last" -gt 0 ] && cursor=$(printf %012d "$last")
    echo "{\"replies\":[$out],\"reply_cursor\":\"$cursor\"}" ;;
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
            "receipts --all-replies",
            "receipts --after 000000000002 --all-replies",
            "receipts --after 000000000002 --all-replies"
        ],
        "the cursor is the newest reply before sending, carried forward"
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
