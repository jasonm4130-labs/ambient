//! The assistant loop against a real sessions folder, a scripted model and a
//! speaker that records instead of speaking.

use super::*;
use crate::config::Config;
use std::cell::RefCell;

/// Answers jump-in questions with `verdict` and reply requests with `reply`,
/// and remembers what it was asked.
struct Scripted {
    verdict: RefCell<String>,
    reply: String,
    asked: RefCell<Vec<Request>>,
}

impl Scripted {
    fn new(verdict: &str, reply: &str) -> Self {
        Self {
            verdict: RefCell::new(verdict.into()),
            reply: reply.into(),
            asked: RefCell::new(Vec::new()),
        }
    }
    fn jump_ins(&self) -> usize {
        self.asked
            .borrow()
            .iter()
            .filter(|r| r.system.contains("You decide whether"))
            .count()
    }
    fn replies(&self) -> usize {
        self.asked.borrow().len() - self.jump_ins()
    }
}

impl Model for Scripted {
    fn complete(&self, request: &Request) -> Result<llm::Completion> {
        self.asked.borrow_mut().push(request.clone());
        let text = if request.system.contains("You decide whether") {
            self.verdict.borrow().clone()
        } else {
            self.reply.clone()
        };
        Ok(llm::Completion {
            text,
            latency: Duration::from_millis(5),
            cost: Some(0.0001),
        })
    }
}

#[derive(Default)]
struct Recorder {
    said: Vec<(String, String)>,
    stops: u32,
}

impl Speaker for Recorder {
    fn say(&mut self, text: &str, emotion: &str) -> Result<()> {
        self.said.push((text.into(), emotion.into()));
        Ok(())
    }
    fn stop(&mut self) {
        self.stops += 1;
    }
}

const YES: &str = r#"{"speak": true, "confidence": 0.9, "reason": "asked by name"}"#;
const NO: &str = r#"{"speak": false, "confidence": 0.9, "reason": "small talk"}"#;
const REPLY: &str = r#"{"text": "It closed on Tuesday.", "emotion": "warm"}"#;

struct Room {
    paths: Paths,
    root: PathBuf,
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
            paths: Paths {
                config_file,
                roster_file: base.join("roster.json"),
                sessions_root: Some(root.clone()),
            },
            root,
        }
    }

    fn paths(&self) -> Paths {
        Paths {
            config_file: self.paths.config_file.clone(),
            roster_file: self.paths.roster_file.clone(),
            sessions_root: self.paths.sessions_root.clone(),
        }
    }

    /// A session that reads as being recorded right now.
    fn live(&self, id: &str) -> PathBuf {
        let dir = self.root.join(id);
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        std::fs::write(dir.join("audio").join("room.native.wav"), b"RIFF....").unwrap();
        dir
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
        let mut cfg = Config::load_from(&self.paths.config_file);
        cfg.set(key, value).unwrap();
        cfg.save_to(&self.paths.config_file).unwrap();
    }
}

#[test]
fn off_by_default_it_neither_joins_nor_speaks() {
    let room = Room::new("off", false);
    room.live("m1");
    room.say("m1", 0, "Claude, are you there?");
    let model = Scripted::new(YES, REPLY);
    let mut speaker = Recorder::default();
    let mut a = Assistant::new(room.paths(), None, &model, &mut speaker);
    assert_eq!(a.step(Instant::now()).unwrap(), Step::Off);
    drop(a);
    assert!(speaker.said.is_empty());
    assert_eq!(model.asked.borrow().len(), 0);
}

#[test]
fn joining_announces_the_assistant_before_anything_else_and_shows_the_notice() {
    let room = Room::new("join", true);
    room.live("m1");
    room.say("m1", 0, "Earlier, someone asked Claude something.");
    let model = Scripted::new(YES, REPLY);
    let mut speaker = Recorder::default();
    let mut a = Assistant::new(room.paths(), None, &model, &mut speaker);
    assert_eq!(a.step(Instant::now()).unwrap(), Step::Joined("m1".into()));
    assert!(
        listening(&room.paths.config_file).is_some(),
        "heartbeat written"
    );
    // Lines from before it joined are context, not a cue to speak.
    assert_eq!(a.step(Instant::now()).unwrap(), Step::Quiet);
    drop(a);
    assert_eq!(speaker.said.len(), 1);
    assert!(speaker.said[0]
        .0
        .contains("AI assistant called Claude is listening"));
    assert_eq!(model.asked.borrow().len(), 0);
}

#[test]
fn a_confident_yes_writes_and_speaks_a_reply_and_logs_it() {
    let room = Room::new("speak", true);
    let dir = room.live("m1");
    let model = Scripted::new(YES, REPLY);
    let mut speaker = Recorder::default();
    let mut a = Assistant::new(room.paths(), None, &model, &mut speaker);
    let t = Instant::now();
    a.step(t).unwrap();
    room.say("m1", 5_000, "Claude, when did the deploy ticket close?");
    assert_eq!(
        a.step(t).unwrap(),
        Step::Spoke("It closed on Tuesday.".into())
    );
    drop(a);
    assert_eq!(
        speaker.said[1],
        ("It closed on Tuesday.".into(), "warm".into())
    );
    let asked = model.asked.borrow();
    assert_eq!(asked[0].model, "anthropic/claude-haiku-5.5");
    assert!(asked[0]
        .user
        .contains("[00:05] someone in the room: Claude, when did"));
    assert_eq!(asked[1].model, "anthropic/claude-sonnet-5.5");
    let log = std::fs::read_to_string(dir.join("assistant.jsonl")).unwrap();
    assert!(
        log.contains(r#""event":"jump-in""#) && log.contains(r#""event":"reply""#),
        "{log}"
    );
}

#[test]
fn a_no_or_a_timid_yes_costs_one_cheap_call_and_says_nothing() {
    let room = Room::new("quiet", true);
    room.live("m1");
    let model = Scripted::new(NO, REPLY);
    let mut speaker = Recorder::default();
    let mut a = Assistant::new(room.paths(), None, &model, &mut speaker);
    let t = Instant::now();
    a.step(t).unwrap();
    room.say("m1", 1_000, "Nice weather today.");
    assert_eq!(a.step(t).unwrap(), Step::Held("nothing to add".into()));
    *model.verdict.borrow_mut() = r#"{"speak": true, "confidence": 0.5, "reason": "maybe"}"#.into();
    room.say("m1", 2_000, "Anyway, where were we?");
    assert!(
        matches!(a.step(t + MIN_INTERVAL).unwrap(), Step::Held(h) if h.contains("below threshold"))
    );
    drop(a);
    assert_eq!(speaker.said.len(), 1, "only the notice");
    assert_eq!((model.jump_ins(), model.replies()), (2, 0));
}

#[test]
fn nothing_new_means_no_call_at_all() {
    let room = Room::new("idle", true);
    room.live("m1");
    let model = Scripted::new(YES, REPLY);
    let mut speaker = Recorder::default();
    let mut a = Assistant::new(room.paths(), None, &model, &mut speaker);
    let t = Instant::now();
    a.step(t).unwrap();
    for i in 1..5 {
        assert_eq!(
            a.step(t + Duration::from_secs(i * 10)).unwrap(),
            Step::Quiet
        );
    }
    drop(a);
    assert_eq!(model.asked.borrow().len(), 0);
}

#[test]
fn cooldown_and_cap_hold_it_back_without_calling_the_model() {
    let room = Room::new("cap", true);
    room.live("m1");
    let model = Scripted::new(YES, REPLY);
    let mut speaker = Recorder::default();
    let mut a = Assistant::new(room.paths(), None, &model, &mut speaker);
    let t = Instant::now();
    a.step(t).unwrap();
    room.say("m1", 1_000, "Claude, one?");
    assert!(matches!(a.step(t).unwrap(), Step::Spoke(_)));
    room.say("m1", 2_000, "Claude, two?");
    assert!(
        matches!(a.step(t + Duration::from_secs(10)).unwrap(), Step::Held(h) if h.contains("cooling down"))
    );
    room.say("m1", 3_000, "Claude, three?");
    assert!(matches!(
        a.step(t + Duration::from_secs(61)).unwrap(),
        Step::Spoke(_)
    ));
    room.say("m1", 4_000, "Claude, four?");
    assert_eq!(
        a.step(t + Duration::from_secs(3600)).unwrap(),
        Step::Held("meeting cap reached".into())
    );
    drop(a);
    assert_eq!(model.jump_ins(), 2, "held steps never reach the model");
    assert_eq!(speaker.said.len(), 3, "notice plus two replies");
}

#[test]
fn its_own_voice_heard_back_is_not_a_cue() {
    let room = Room::new("echo", true);
    room.live("m1");
    let model = Scripted::new(YES, REPLY);
    let mut speaker = Recorder::default();
    let mut a = Assistant::new(room.paths(), None, &model, &mut speaker);
    let t = Instant::now();
    a.step(t).unwrap();
    room.say(
        "m1",
        1_000,
        "an AI assistant called Claude is listening to this meeting",
    );
    assert_eq!(a.step(t).unwrap(), Step::Quiet);
    drop(a);
    assert_eq!(model.asked.borrow().len(), 0);
}

#[test]
fn turning_it_off_mid_meeting_leaves_and_clears_the_notice() {
    let room = Room::new("toggle", true);
    room.live("m1");
    let model = Scripted::new(YES, REPLY);
    let mut speaker = Recorder::default();
    let mut a = Assistant::new(room.paths(), None, &model, &mut speaker);
    a.step(Instant::now()).unwrap();
    assert!(listening(&room.paths.config_file).is_some());
    room.set("assistant", "off");
    room.say("m1", 1_000, "Claude, are you still there?");
    assert_eq!(a.step(Instant::now()).unwrap(), Step::Off);
    assert!(listening(&room.paths.config_file).is_none());
    drop(a);
    assert_eq!(speaker.stops, 1, "the voice helper is stopped");
    assert_eq!(model.asked.borrow().len(), 0);
}

#[test]
fn a_new_meeting_gets_a_fresh_cap_and_a_fresh_notice() {
    let room = Room::new("next", true);
    room.set("assistant.max_per_meeting", "1");
    let first = room.live("m1");
    let model = Scripted::new(YES, REPLY);
    let mut speaker = Recorder::default();
    let mut a = Assistant::new(room.paths(), None, &model, &mut speaker);
    let t = Instant::now();
    a.step(t).unwrap();
    room.say("m1", 1_000, "Claude, one?");
    assert!(matches!(a.step(t).unwrap(), Step::Spoke(_)));
    // The first recording stops (its scratch wav goes stale) and a second starts.
    std::fs::remove_file(first.join("audio").join("room.native.wav")).unwrap();
    assert_eq!(a.step(t).unwrap(), Step::NoMeeting);
    room.live("m2");
    assert_eq!(a.step(t).unwrap(), Step::Joined("m2".into()));
    room.say("m2", 1_000, "Claude, are you here too?");
    assert!(matches!(
        a.step(t + Duration::from_secs(1)).unwrap(),
        Step::Spoke(_)
    ));
    drop(a);
    let notices = speaker
        .said
        .iter()
        .filter(|(s, _)| s.contains("is listening"))
        .count();
    assert_eq!(notices, 2);
}

#[test]
fn a_failing_model_keeps_the_assistant_quiet_and_running() {
    struct Down;
    impl Model for Down {
        fn complete(&self, _: &Request) -> Result<llm::Completion> {
            Err(anyhow!("503 no ZDR provider"))
        }
    }
    let room = Room::new("down", true);
    let dir = room.live("m1");
    let mut speaker = Recorder::default();
    let mut a = Assistant::new(room.paths(), None, &Down, &mut speaker);
    let t = Instant::now();
    a.step(t).unwrap();
    room.say("m1", 1_000, "Claude?");
    assert_eq!(
        a.step(t).unwrap(),
        Step::Held("the jump-in call failed".into())
    );
    drop(a);
    assert_eq!(speaker.said.len(), 1, "only the notice");
    let log = std::fs::read_to_string(dir.join("assistant.jsonl")).unwrap();
    assert!(log.contains("no ZDR provider"), "{log}");
}

#[test]
fn following_one_session_ignores_another_that_is_live() {
    let room = Room::new("only", true);
    room.live("other");
    let model = Scripted::new(YES, REPLY);
    let mut speaker = Recorder::default();
    let mut a = Assistant::new(room.paths(), Some("mine".into()), &model, &mut speaker);
    assert_eq!(a.step(Instant::now()).unwrap(), Step::NoMeeting);
    drop(a);
    assert!(speaker.said.is_empty());
}
