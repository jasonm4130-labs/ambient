use super::*;
use std::path::PathBuf;

fn cfg(enabled: bool, home: bool) -> TalkConfig {
    TalkConfig {
        enabled,
        speak: true,
        firstmate_home: home.then(|| PathBuf::from("/tmp/fm")),
    }
}

fn ev(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

/// Wait for the workers to report, up to a few seconds.
fn settle(talk: &mut Talk, until: impl Fn(&Talk) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !until(talk) && Instant::now() < deadline {
        talk.poll();
        std::thread::sleep(Duration::from_millis(20));
    }
    talk.poll();
}

fn sh(script: &str) -> Command {
    let mut c = Command::new("/bin/sh");
    c.args(["-c", script]);
    c
}

#[test]
fn a_spoken_turn_shows_both_sides_and_ends_done() {
    let mut t = Turn::new(1, true);
    t.apply(&ev(
        r#"{"event":"heard","text":"what's merged?","speech_s":1.2}"#,
    ));
    assert_eq!(t.stage, Stage::Sending);
    assert_eq!(t.you.as_deref(), Some("what's merged?"));
    t.apply(&ev(r#"{"event":"sent","id":"n1","announced":true}"#));
    assert_eq!(t.stage, Stage::Waiting);
    assert_eq!(t.note, None);
    t.apply(&ev(
        r#"{"event":"reply","id":"n1","text":"PR 12 merged.","speech":"PR 12 merged."}"#,
    ));
    assert_eq!(t.stage, Stage::Speaking);
    assert_eq!(t.reply.as_deref(), Some("PR 12 merged."));
    t.apply(&ev(
        r#"{"event":"spoken","first_audio_ms":60,"audio_s":1.0}"#,
    ));
    assert_eq!(t.stage, Stage::Done);
    t.exited(Some(0), "");
    assert_eq!(
        t.stage,
        Stage::Done,
        "a clean exit leaves a closed turn alone"
    );
}

#[test]
fn with_speaking_off_the_reply_is_done_as_soon_as_it_arrives() {
    let mut t = Turn::new(1, false);
    t.apply(&ev(r#"{"event":"heard","text":"hi"}"#));
    t.apply(&ev(r#"{"event":"reply","id":"n1","text":"hello"}"#));
    assert_eq!(t.stage, Stage::Done);
}

#[test]
fn nothing_heard_queued_unannounced_and_text_only_are_said() {
    let mut t = Turn::new(1, true);
    t.apply(&ev(r#"{"event":"nothing","speech_s":0.1}"#));
    assert_eq!(t.stage, Stage::Nothing);
    assert!(!t.stage.is_open());

    let mut t = Turn::new(2, true);
    t.apply(&ev(r#"{"event":"heard","text":"hi"}"#));
    t.apply(&ev(
        r#"{"event":"queued","reason":"firstmate is not taking notes"}"#,
    ));
    assert!(t.note.as_deref().unwrap().starts_with("Queued: firstmate"));
    t.apply(&ev(r#"{"event":"sent","id":"n2","announced":false}"#));
    assert!(t.note.as_deref().unwrap().contains("not woken"));
    t.apply(&ev(r#"{"event":"reply","id":"n2","text":"ok"}"#));
    assert_eq!(t.note, None, "a reply clears the waiting notes");
    t.apply(&ev(r#"{"event":"text_only","reason":"the voice failed"}"#));
    assert_eq!(t.stage, Stage::Done);
    assert_eq!(t.note.as_deref(), Some("Not said aloud: the voice failed"));
}

#[test]
fn a_worker_that_dies_open_is_failed_with_its_last_words() {
    let mut t = Turn::new(1, true);
    t.exited(
        Some(1),
        "  talk: loading\nError: a recording is in progress; talking is paused until it stops\n",
    );
    assert_eq!(t.stage, Stage::Failed);
    assert_eq!(
        t.note.as_deref(),
        Some("a recording is in progress; talking is paused until it stops")
    );
}

#[test]
fn a_signalled_worker_is_stopped_not_failed() {
    let mut t = Turn::new(1, true);
    t.apply(&ev(r#"{"event":"heard","text":"hi"}"#));
    t.apply(&ev(r#"{"event":"reply","id":"n","text":"long answer"}"#));
    t.exited(Some(130), "");
    assert_eq!(t.stage, Stage::Stopped);
    assert_eq!(t.reply.as_deref(), Some("long answer"));

    let mut t = Turn::new(2, true);
    t.apply(&ev(r#"{"event":"sent","id":"n","announced":true}"#));
    t.exited(None, "");
    assert_eq!(t.stage, Stage::Stopped);
    assert!(t.note.as_deref().unwrap().contains("inbox"));
}

/// D3: the microphone is the meeting's while a recording runs or a call is
/// waiting to be recorded.
#[test]
fn talking_is_refused_while_recording_or_armed_or_unset() {
    let on = cfg(true, true);
    assert_eq!(refusal(&on, PhaseKind::Idle), None);
    assert_eq!(refusal(&on, PhaseKind::Failed), None);
    for busy in [PhaseKind::Recording, PhaseKind::Stopping] {
        assert!(refusal(&on, busy).unwrap().contains("recording"));
    }
    assert!(refusal(&on, PhaseKind::Armed).unwrap().contains("call"));
    assert!(refusal(&cfg(false, true), PhaseKind::Idle)
        .unwrap()
        .contains("off"));
    assert!(refusal(&cfg(true, false), PhaseKind::Idle)
        .unwrap()
        .contains("home"));
    assert_eq!(
        pause_reason(PhaseKind::Armed),
        refusal(&on, PhaseKind::Armed)
    );
    assert_eq!(pause_reason(PhaseKind::Idle), None);
    assert_eq!(paused(PhaseKind::Armed), Some("armed"));
    assert_eq!(paused(PhaseKind::Idle), None);
}

#[test]
fn the_menu_line_follows_the_latest_turn_and_a_notice_wins() {
    let mut talk = Talk::default();
    assert_eq!(talk.menu_line(), None);
    let mut t = Turn::new(1, true);
    t.apply(&ev(r#"{"event":"heard","text":"hi"}"#));
    t.apply(&ev(
        r#"{"event":"reply","id":"n","text":"\n**All** green.\nMore detail."}"#,
    ));
    talk.turns.push_back(t);
    assert_eq!(talk.menu_line().as_deref(), Some("firstmate: All green."));
    assert_eq!(talk.symbol(), Some(SPEAKING_SYMBOL));
    talk.notice("Talking is paused while Ambient is recording.");
    assert_eq!(
        talk.menu_line().as_deref(),
        Some("Talking is paused while Ambient is recording.")
    );
}

#[test]
fn the_payload_carries_both_sides_and_the_pause() {
    let mut talk = Talk::default();
    let mut t = Turn::new(7, true);
    t.apply(&ev(r#"{"event":"heard","text":"status?"}"#));
    talk.turns.push_back(t);
    let p = talk.payload(
        &cfg(true, true),
        Err("another app already uses ⌥Space"),
        PhaseKind::Recording,
    );
    assert_eq!(p["enabled"], json!(true));
    assert_eq!(p["configured"], json!(true));
    assert_eq!(p["paused"], json!("recording"));
    assert_eq!(p["hotkey_error"], json!("another app already uses ⌥Space"));
    assert_eq!(p["listening"], json!(false));
    assert_eq!(p["turns"][0]["you"], json!("status?"));
    assert_eq!(p["turns"][0]["state"], json!("sending"));
    assert_eq!(p["turns"][0]["reply"], Value::Null);
}

/// The worker gets every sample on stdin, and its events become the turn.
#[test]
fn a_worker_hears_the_hold_on_stdin_and_its_events_become_the_turn() {
    let mut talk = Talk::default();
    let samples = vec![0.5f32; 1000];
    // Counts the bytes it was given, then plays a whole turn.
    let script = r#"n=$(wc -c | tr -d ' ')
echo "{\"event\":\"heard\",\"text\":\"$n bytes\"}"
echo '{"event":"sent","id":"n1","announced":true}'
echo '{"event":"reply","id":"n1","text":"got it"}'
echo '{"event":"dry_run","would_say":"got it"}'"#;
    let id = talk.start(sh(script), pcm_bytes(&samples), false).unwrap();
    settle(&mut talk, |t| t.pids.is_empty());
    let turn = talk.turns.iter().find(|t| t.id == id).unwrap();
    assert_eq!(turn.you.as_deref(), Some("4000 bytes"));
    assert_eq!(turn.reply.as_deref(), Some("got it"));
    assert_eq!(turn.stage, Stage::Done);
}

#[test]
fn holding_the_key_again_stops_a_reply_mid_word() {
    let mut talk = Talk::default();
    // Exits 130 on SIGTERM, as the real worker's handler does.
    let script = r#"trap 'exit 130' TERM
cat >/dev/null
echo '{"event":"heard","text":"hi"}'
echo '{"event":"reply","id":"n1","text":"a long answer"}'
while :; do sleep 0.05; done"#;
    talk.start(sh(script), Vec::new(), true).unwrap();
    settle(&mut talk, Talk::speaking);
    assert!(talk.speaking());
    talk.barge_in();
    settle(&mut talk, |t| t.pids.is_empty());
    let turn = talk.turns.back().unwrap();
    assert_eq!(turn.stage, Stage::Stopped, "{turn:?}");
    assert!(!talk.speaking());
    assert_eq!(talk.symbol(), None);
}

/// D3: a call arming while a reply is being said stops it, but the reply is
/// still shown.
#[test]
fn arming_while_a_reply_is_said_stops_it_and_keeps_the_text() {
    let mut talk = Talk::default();
    // Exits 130 on SIGUSR1 while speaking, as the real worker's handler does.
    let script = r#"trap 'exit 130' USR1
cat >/dev/null
echo '{"event":"heard","text":"hi"}'
echo '{"event":"reply","id":"n1","text":"PR 12 merged."}'
while :; do sleep 0.05; done"#;
    talk.start(sh(script), Vec::new(), true).unwrap();
    settle(&mut talk, Talk::speaking);
    talk.hush();
    settle(&mut talk, |t| t.pids.is_empty());
    let turn = talk.turns.back().unwrap();
    assert_eq!(turn.stage, Stage::Stopped, "{turn:?}");
    assert_eq!(turn.reply.as_deref(), Some("PR 12 merged."));
    assert_eq!(
        talk.menu_line().as_deref(),
        Some("firstmate: PR 12 merged.")
    );
    assert!(!talk.speaking());
}

#[test]
fn only_the_last_turns_are_kept() {
    let mut talk = Talk::default();
    for _ in 0..MAX_TURNS + 3 {
        talk.start(sh("cat >/dev/null"), Vec::new(), true).unwrap();
    }
    settle(&mut talk, |t| t.pids.is_empty());
    assert_eq!(talk.turns.len(), MAX_TURNS);
    assert_eq!(talk.turns.front().unwrap().id, 4);
}
