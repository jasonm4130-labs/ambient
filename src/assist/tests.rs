//! The watching tools against a real sessions folder and a speaker that
//! records instead of speaking.

use super::*;
use crate::config::Config;
use std::cell::RefCell;
use std::rc::Rc;

/// What the recording speaker heard, shared with the test after the speaker
/// itself has been handed to the [`Watch`].
#[derive(Default)]
struct Heard {
    said: Vec<(String, String)>,
    stops: u32,
    built: u32,
    /// Fail every `say` from now on.
    broken: bool,
}

struct Recorder(Rc<RefCell<Heard>>);

impl Speaker for Recorder {
    fn say(&mut self, text: &str, emotion: &str) -> Result<voice::Spoken> {
        let mut h = self.0.borrow_mut();
        if h.broken {
            anyhow::bail!("the helper died");
        }
        h.said.push((text.into(), emotion.into()));
        Ok(voice::Spoken {
            first_audio_ms: Some(50),
            audio_s: 2.0,
        })
    }
    fn stop(&mut self) {
        self.0.borrow_mut().stops += 1;
    }
}

struct Room {
    base: PathBuf,
    root: PathBuf,
    config_file: PathBuf,
    heard: Rc<RefCell<Heard>>,
}

impl Room {
    /// A config with the assistant on (unless `enabled` is false) and a
    /// sessions folder holding nothing yet.
    fn new(test: &str, enabled: bool) -> Self {
        let base =
            std::env::temp_dir().join(format!("ambient-assist-{test}-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let root = base.join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        let config_file = base.join("config.json");
        let mut cfg = Config {
            sessions_dir: Some(root.clone()),
            ..Default::default()
        };
        cfg.assistant.enabled = enabled;
        cfg.assistant.cooldown_s = 60;
        cfg.assistant.max_per_meeting = 2;
        cfg.save_to(&config_file).unwrap();
        Room {
            base,
            root,
            config_file,
            heard: Rc::default(),
        }
    }

    fn watch(&self) -> Watch {
        let heard = Rc::clone(&self.heard);
        let make: MakeSpeaker = Box::new(move |_| {
            heard.borrow_mut().built += 1;
            Box::new(Recorder(Rc::clone(&heard)))
        });
        let paths = Paths {
            config_file: self.config_file.clone(),
            roster_file: self.base.join("roster.json"),
            sessions_root: Some(self.root.clone()),
        };
        Watch::new(paths, make).with_pacing(Pacing {
            poll: Duration::from_millis(10),
            settle: Duration::from_millis(60),
            longest: Duration::from_millis(400),
            speaking_wait: Duration::from_millis(100),
        })
    }

    /// A session that reads as being recorded right now.
    fn live(&self, id: &str) -> PathBuf {
        let dir = self.root.join(id);
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        std::fs::write(dir.join("audio").join("room.native.wav"), b"RIFF....").unwrap();
        dir
    }

    fn end(&self, id: &str) {
        std::fs::remove_file(self.root.join(id).join("audio").join("room.native.wav")).unwrap();
    }

    fn say(&self, id: &str, start_ms: u64, text: &str) {
        let line = json!({
            "track": "room", "start_ms": start_ms, "end_ms": start_ms + 1000,
            "text": text, "confidence": 0.9,
        });
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join(id).join("raw.jsonl"))
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }

    fn set(&self, key: &str, value: &str) {
        let mut cfg = Config::load_from(&self.config_file);
        cfg.set(key, value).unwrap();
        cfg.save_to(&self.config_file).unwrap();
    }

    fn heartbeat(&self) -> Option<Value> {
        let text = std::fs::read_to_string(Watch::heartbeat_path(&self.config_file)).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn said(&self) -> Vec<String> {
        self.heard
            .borrow()
            .said
            .iter()
            .map(|(t, _)| t.clone())
            .collect()
    }

    fn events(&self, id: &str) -> Vec<Value> {
        std::fs::read_to_string(self.root.join(id).join("assistant.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

fn lines(v: &Value) -> Vec<String> {
    v["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_string())
        .collect()
}

const SHORT: Duration = Duration::from_millis(150);

#[test]
fn off_by_default_it_will_not_watch_or_speak() {
    let room = Room::new("off", false);
    room.live("m1");
    room.say("m1", 0, "Claude, are you there?");
    let mut w = room.watch();
    let refused = w.watch(None).unwrap_err();
    assert!(refused.contains("turned off"), "{refused}");
    assert!(w.wait(SHORT).is_err(), "no waiting without watching");
    assert!(w.speak("Hello.", None).is_err());
    assert!(room.said().is_empty());
    assert_eq!(room.heard.borrow().built, 0, "no voice is even loaded");
    assert!(room.heartbeat().is_none());
}

#[test]
fn with_no_meeting_there_is_nothing_to_watch() {
    let room = Room::new("nomeeting", true);
    let refused = room.watch().watch(None).unwrap_err();
    assert!(refused.contains("not recording"), "{refused}");
    assert!(room.said().is_empty());
}

#[test]
fn watching_announces_the_assistant_first_shows_the_notice_and_hands_back_context() {
    let room = Room::new("join", true);
    room.live("m1");
    room.say("m1", 1_000, "Let's start with the roadmap.");
    let mut w = room.watch();
    let v = w.watch(None).unwrap();
    assert_eq!(v["watching"], "m1");
    assert_eq!(room.said(), [consent_notice("Claude")]);
    assert_eq!(
        v["recent"],
        json!(["[00:01] someone in the room: Let's start with the roadmap."])
    );
    let beat = room.heartbeat().expect("the menu bar is told");
    assert_eq!(beat["session"], "m1");
    assert_eq!(beat["state"], "listening");
    assert_eq!(listening(&room.config_file).as_deref(), Some("listening"));
    assert_eq!(room.events("m1")[0]["event"], "joined");
    // Asking again is not a second announcement.
    w.watch(None).unwrap();
    assert_eq!(room.said().len(), 1);
    // What was said before it joined is context, not news.
    assert!(lines(&w.wait(SHORT).unwrap()).is_empty());
}

#[test]
fn a_notice_that_cannot_be_spoken_means_no_watching() {
    let room = Room::new("mute", true);
    room.live("m1");
    room.heard.borrow_mut().broken = true;
    let mut w = room.watch();
    let refused = w.watch(None).unwrap_err();
    assert!(refused.contains("listening notice"), "{refused}");
    assert!(w.wait(SHORT).is_err());
    assert!(room.heartbeat().is_none());
}

#[test]
fn waiting_returns_a_burst_once_the_room_pauses_and_nothing_on_a_timeout() {
    let room = Room::new("wait", true);
    room.live("m1");
    let mut w = room.watch();
    w.watch(None).unwrap();
    let quiet = w.wait(SHORT).unwrap();
    assert_eq!(quiet["status"], "live");
    assert!(lines(&quiet).is_empty());
    room.say("m1", 5_000, "Did the deploy go out?");
    room.say("m1", 7_000, "I think so, not sure.");
    let v = w.wait(Duration::from_secs(5)).unwrap();
    assert_eq!(
        lines(&v),
        [
            "[00:05] someone in the room: Did the deploy go out?",
            "[00:07] someone in the room: I think so, not sure."
        ]
    );
    assert_eq!(v["may_speak"], "yes");
    // The cursor moved: the same lines do not come back.
    assert!(lines(&w.wait(SHORT).unwrap()).is_empty());
}

#[test]
fn its_name_returns_at_once_without_waiting_for_a_pause() {
    let room = Room::new("named", true);
    room.live("m1");
    let mut w = room.watch().with_pacing(Pacing {
        poll: Duration::from_millis(10),
        settle: Duration::from_secs(30),
        longest: Duration::from_secs(30),
        ..Pacing::default()
    });
    w.watch(None).unwrap();
    room.say("m1", 2_000, "Claude, when did that ticket close?");
    let started = Instant::now();
    let v = w.wait(Duration::from_secs(20)).unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(v["named_you"], true);
    assert_eq!(lines(&v).len(), 1);
}

#[test]
fn a_question_by_name_right_after_the_notice_is_not_taken_for_its_echo() {
    let room = Room::new("after-notice", true);
    room.live("m1");
    let mut w = room.watch();
    w.watch(None).unwrap();
    room.say("m1", 2_000, "Claude, are you listening?");
    let v = w.wait(Duration::from_secs(5)).unwrap();
    assert_eq!(v["named_you"], true);
    assert_eq!(
        lines(&v),
        ["[00:02] someone in the room: Claude, are you listening?"]
    );
}

#[test]
fn the_notice_heard_back_is_left_out() {
    let room = Room::new("notice-echo", true);
    room.live("m1");
    let mut w = room.watch();
    w.watch(None).unwrap();
    let notice = room.said().last().unwrap().clone();
    room.say("m1", 2_000, &notice);
    let v = w.wait(SHORT).unwrap();
    assert!(lines(&v).is_empty(), "{v}");
    assert_eq!(v["left_out_as_your_own_voice"], 1);
}

#[test]
fn speaking_is_said_logged_and_then_held_by_the_cooldown() {
    let room = Room::new("speak", true);
    room.live("m1");
    let mut w = room.watch();
    w.watch(None).unwrap();
    let v = w.speak("It closed on Tuesday.", Some("warm")).unwrap();
    assert_eq!(v["may_still_speak"], 1);
    assert_eq!(room.said().last().unwrap(), "It closed on Tuesday.");
    assert_eq!(room.heard.borrow().said.last().unwrap().1, "warm");
    let spoke = room.events("m1");
    assert_eq!(spoke.last().unwrap()["event"], "spoke");
    assert_eq!(spoke.last().unwrap()["text"], "It closed on Tuesday.");
    // Inside the cooldown, refused, and the agent is told why.
    let held = w.speak("Also, the build is green.", None).unwrap_err();
    assert!(held.contains("cooling down"), "{held}");
    let v = w.wait(SHORT).unwrap();
    assert!(
        v["may_speak"].as_str().unwrap().starts_with("not now"),
        "{v}"
    );
    assert_eq!(room.said().len(), 2, "the notice and one reply");
}

#[test]
fn a_remark_is_not_said_over_a_talk_reply_and_not_counted() {
    let room = Room::new("talking", true);
    room.live("m1");
    let mut w = room.watch();
    w.watch(None).unwrap();
    let lock = voice::SpeakingLock::path(&room.config_file);
    let talk = voice::SpeakingLock::acquire(&lock, "talk", Duration::ZERO).unwrap();
    let held = w.speak("It closed on Tuesday.", None).unwrap_err();
    assert!(held.contains("talk") && held.contains("speaking"), "{held}");
    assert_eq!(room.said().len(), 1, "only the notice");
    drop(talk);
    let v = w.speak("It closed on Tuesday.", None).unwrap();
    assert_eq!(
        v["may_still_speak"], 1,
        "the refused remark was not counted"
    );
    assert!(!lock.exists(), "the assistant let go of the voice");
}

#[test]
fn the_cap_holds_even_without_a_cooldown() {
    let room = Room::new("cap", true);
    room.set("assistant.cooldown_s", "0");
    room.live("m1");
    let mut w = room.watch();
    w.watch(None).unwrap();
    w.speak("One.", None).unwrap();
    w.speak("Two.", None).unwrap();
    let held = w.speak("Three.", None).unwrap_err();
    assert!(held.contains("as often as one meeting allows"), "{held}");
}

#[test]
fn watching_the_same_meeting_again_keeps_its_cooldown_and_count() {
    let room = Room::new("rewatch", true);
    room.live("m1");
    let mut w = room.watch();
    w.watch(None).unwrap();
    w.speak("It closed on Tuesday.", None).unwrap();
    w.stop_watching();
    let v = w.watch(None).unwrap();
    assert_eq!(v["limits"]["spoken"], 1, "{v}");
    let held = w.speak("Also, the build is green.", None).unwrap_err();
    assert!(held.contains("cooling down"), "{held}");
    assert_eq!(
        room.said(),
        [
            consent_notice("Claude"),
            "It closed on Tuesday.".to_string(),
            consent_notice("Claude"),
        ],
        "the notice is spoken again on each new watch"
    );
}

#[test]
fn the_cap_reached_in_one_watch_holds_in_another() {
    let room = Room::new("capshared", true);
    room.set("assistant.cooldown_s", "0");
    room.live("m1");
    let mut first = room.watch();
    first.watch(None).unwrap();
    first.speak("One.", None).unwrap();
    room.heard.borrow_mut().broken = true;
    first.speak("Two.", None).unwrap_err();
    room.heard.borrow_mut().broken = false;
    let mut second = room.watch();
    let v = second.watch(None).unwrap();
    assert_eq!(v["limits"]["spoken"], 2, "{v}");
    let held = second.speak("Three.", None).unwrap_err();
    assert!(held.contains("as often as one meeting allows"), "{held}");
}

#[test]
fn two_watches_on_one_meeting_share_the_cooldown_while_both_watch() {
    let room = Room::new("twowatch", true);
    room.live("m1");
    let mut a = room.watch();
    let mut b = room.watch();
    a.watch(None).unwrap();
    b.watch(None).unwrap();
    a.speak("It closed on Tuesday.", None).unwrap();
    let held = b.speak("Also, the build is green.", None).unwrap_err();
    assert!(held.contains("cooling down"), "{held}");
    let v = b.wait(SHORT).unwrap();
    assert!(
        v["may_speak"].as_str().unwrap().starts_with("not now"),
        "{v}"
    );
    assert_eq!(v["spoken"], 1, "{v}");
}

#[test]
fn two_watches_on_one_meeting_share_the_cap_while_both_watch() {
    let room = Room::new("twocap", true);
    room.set("assistant.cooldown_s", "0");
    room.live("m1");
    let mut a = room.watch();
    let mut b = room.watch();
    a.watch(None).unwrap();
    b.watch(None).unwrap();
    a.speak("One.", None).unwrap();
    b.speak("Two.", None).unwrap();
    let held = a.speak("Three.", None).unwrap_err();
    assert!(held.contains("as often as one meeting allows"), "{held}");
}

#[test]
fn what_it_says_must_be_short_plain_speech_in_a_known_tone() {
    let room = Room::new("shape", true);
    room.live("m1");
    let mut w = room.watch();
    w.watch(None).unwrap();
    assert!(w.speak("   ", None).is_err());
    assert!(w
        .speak(&"word ".repeat(200), None)
        .unwrap_err()
        .contains("Too long"));
    assert!(w
        .speak("Hi.", Some("furious"))
        .unwrap_err()
        .contains("Unknown emotion"));
    assert_eq!(room.said().len(), 1, "only the notice");
}

#[test]
fn its_own_voice_heard_back_is_left_out() {
    let room = Room::new("echo", true);
    room.live("m1");
    let mut w = room.watch();
    w.watch(None).unwrap();
    w.speak(
        "That ticket was closed on Tuesday, so you can drop it.",
        None,
    )
    .unwrap();
    room.say(
        "m1",
        9_000,
        "that ticket was closed on tuesday so you can drop it",
    );
    room.say("m1", 12_000, "Great, thanks.");
    let v = w.wait(Duration::from_secs(5)).unwrap();
    assert_eq!(lines(&v), ["[00:12] someone in the room: Great, thanks."]);
    assert_eq!(v["left_out_as_your_own_voice"], 1);
}

#[test]
fn turning_it_off_mid_meeting_ends_the_watch_and_the_notice() {
    let room = Room::new("toggle", true);
    room.live("m1");
    let mut w = room.watch();
    w.watch(None).unwrap();
    assert!(room.heartbeat().is_some());
    room.set("assistant", "off");
    let v = w.wait(Duration::from_secs(5)).unwrap();
    assert_eq!(v["status"], "off");
    assert!(room.heartbeat().is_none());
    assert_eq!(room.heard.borrow().stops, 1, "the voice is unloaded");
    assert!(w
        .speak("Still here?", None)
        .unwrap_err()
        .contains("turned off"));
    assert_eq!(room.events("m1").last().unwrap()["event"], "left");
}

#[test]
fn the_meeting_ending_ends_the_watch() {
    let room = Room::new("ended", true);
    room.live("m1");
    let mut w = room.watch();
    w.watch(None).unwrap();
    room.end("m1");
    let v = w.wait(Duration::from_secs(5)).unwrap();
    assert_eq!(v["status"], "ended");
    assert!(room.heartbeat().is_none());
    assert!(w.wait(SHORT).is_err(), "nothing left to wait on");
}

#[test]
fn a_new_meeting_is_announced_again_with_a_fresh_voice_and_limits() {
    let room = Room::new("again", true);
    room.set("assistant.max_per_meeting", "1");
    room.live("m1");
    let mut w = room.watch();
    w.watch(None).unwrap();
    w.speak("First meeting.", None).unwrap();
    room.end("m1");
    assert_eq!(w.wait(SHORT).unwrap()["status"], "ended");
    room.live("m2");
    w.watch(None).unwrap();
    assert_eq!(
        room.heard.borrow().built,
        2,
        "built from the settings of the day"
    );
    w.speak("Second meeting.", None).unwrap();
    assert_eq!(
        room.said(),
        [
            consent_notice("Claude"),
            "First meeting.".to_string(),
            consent_notice("Claude"),
            "Second meeting.".to_string(),
        ]
    );
}

#[test]
fn only_the_session_being_recorded_can_be_watched() {
    let room = Room::new("which", true);
    room.live("m1");
    std::fs::create_dir_all(room.root.join("old")).unwrap();
    let refused = room.watch().watch(Some("old")).unwrap_err();
    assert!(refused.contains("not being recorded"), "{refused}");
    assert!(room.watch().watch(Some("m1")).is_ok());
}

#[test]
fn a_failed_voice_is_reported_and_still_counts() {
    let room = Room::new("voicefail", true);
    room.live("m1");
    let mut w = room.watch();
    w.watch(None).unwrap();
    room.heard.borrow_mut().broken = true;
    let e = w.speak("Hello.", None).unwrap_err();
    assert!(e.contains("voice failed"), "{e}");
    assert!(w
        .speak("Hello again.", None)
        .unwrap_err()
        .contains("cooling down"));
    assert_eq!(room.events("m1").last().unwrap()["stage"], "voice");
}

#[test]
fn stopping_removes_the_notice_and_unloads_the_voice() {
    let room = Room::new("stop", true);
    room.live("m1");
    let mut w = room.watch();
    w.watch(None).unwrap();
    assert_eq!(w.stop_watching()["stopped_watching"], "m1");
    assert!(room.heartbeat().is_none());
    assert_eq!(room.heard.borrow().stops, 1);
    assert!(w.stop_watching()["stopped_watching"].is_null());
}

#[test]
fn leaving_keeps_a_heartbeat_another_process_wrote() {
    let room = Room::new("otherbeat", true);
    room.live("m1");
    let path = Watch::heartbeat_path(&room.config_file);
    let mut w = room.watch();
    w.watch(None).unwrap();
    let other = json!({"pid": 1, "session": "m1", "state": "listening", "updated_at": 0});
    std::fs::write(&path, other.to_string()).unwrap();
    w.stop_watching();
    assert_eq!(room.heartbeat(), Some(other.clone()));
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(
        room.heartbeat(),
        Some(other),
        "an idle process leaves it alone"
    );
}

#[test]
fn the_notice_lapses_when_the_agent_stops_calling() {
    let room = Room::new("lapse", true);
    let path = Watch::heartbeat_path(&room.config_file);
    let beat = Mutex::new(Beat {
        session: Some("m1".into()),
        state: "listening",
        last_call: Some(Instant::now()),
    });
    write_beat(&path, &beat);
    assert!(listening(&room.config_file).is_some());
    beat.lock().unwrap().last_call = Instant::now().checked_sub(ATTENTION + Duration::from_secs(1));
    write_beat(&path, &beat);
    assert!(!path.exists());
}

#[test]
fn dropping_the_server_leaves_the_meeting() {
    let room = Room::new("drop", true);
    room.live("m1");
    let mut w = room.watch();
    w.watch(None).unwrap();
    drop(w);
    assert!(room.heartbeat().is_none());
    assert_eq!(room.events("m1").last().unwrap()["event"], "left");
}

#[test]
fn the_tools_are_routed_by_name_with_their_arguments() {
    let room = Room::new("route", true);
    room.live("m1");
    let mut w = room.watch();
    assert!(w.call("nope", &json!({})).is_none());
    w.call("watch_meeting", &json!({})).unwrap().unwrap();
    let v = w
        .call("wait_for_transcript", &json!({"timeout_s": 1}))
        .unwrap()
        .unwrap();
    assert_eq!(v["status"], "live");
    w.call("speak", &json!({"text": "Hi.", "emotion": "amused"}))
        .unwrap()
        .unwrap();
    assert_eq!(room.heard.borrow().said.last().unwrap().1, "amused");
    w.call("stop_watching", &json!({})).unwrap().unwrap();
    for tool in Watch::tools() {
        let name = tool["name"].as_str().unwrap();
        assert!(w.call(name, &json!({})).is_some(), "{name} is routed");
    }
}

#[test]
fn the_prompt_names_the_assistant_the_tools_and_what_the_user_added() {
    let p = watch_prompt("Hamish", Some("Listen out for budget numbers."));
    for needle in [
        "You are Hamish",
        "watch_meeting",
        "wait_for_transcript",
        "speak",
        "stop_watching",
        "The user adds: Listen out for budget numbers.",
    ] {
        assert!(p.contains(needle), "missing {needle:?}");
    }
    assert!(!watch_prompt("Claude", Some("  ")).contains("The user adds"));
}

#[test]
fn its_name_counts_only_as_a_whole_word() {
    assert!(mentions("Claude, are you there?", "Claude"));
    assert!(mentions("what does CLAUDE think", "Claude"));
    assert!(mentions("is that Claude's call?", "Claude"));
    assert!(!mentions("Claudette sent the invite", "Claude"));
    assert!(!mentions("no names here", "Claude"));
    assert!(mentions("Ambient Bot, what was the budget?", "Ambient Bot"));
    assert!(mentions("ask ambient  BOT then", "Ambient Bot"));
    assert!(!mentions("the bot in ambient", "Ambient Bot"));
    assert!(!mentions("Ambient Bots are here", "Ambient Bot"));
}

#[test]
fn a_line_names_its_speaker_when_one_is_known_and_its_track_when_not() {
    let named =
        json!({"speaker": "Priya", "track": "call", "start_ms": 65_000, "text": " Merged. "});
    assert_eq!(render(&named), "[01:05] Priya: Merged.");
    let call = json!({"track": "call", "start_ms": 0, "text": "Hello?"});
    assert_eq!(render(&call), "[00:00] someone on the call: Hello?");
    let blank = json!({"speaker": "", "track": "room", "start_ms": 3_000, "text": "Hi."});
    assert_eq!(render(&blank), "[00:03] someone in the room: Hi.");
}
