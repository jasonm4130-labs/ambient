use super::*;

/// A config file of its own per test, in a fresh folder, so the mailbox and
/// heartbeat beside it are the test's alone.
fn config(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ambient-mailbox-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("config.json")
}

const RID: &str = "ambient-00112233445566778899aabb";
const RID2: &str = "ambient-ffeeddccbbaa998877665544";

fn me() -> u32 {
    std::process::id()
}

/// A pid that existed once and does not now: a `true` spawned and reaped.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn a_posted_note_is_open_and_read_back_whole() {
    let cfg = config("post");
    let mb = Mailbox::at(&cfg).unwrap();
    let mode = std::fs::metadata(mb.dir()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "the folder is the owner's alone");
    let note = mb
        .post(
            RID,
            "what's the build\nstatus?",
            me(),
            Duration::from_secs(300),
        )
        .unwrap();
    assert_eq!(
        listing(mb.dir()),
        [format!("{RID}.json")],
        "nothing staged is left"
    );
    let open = mb.open_notes();
    assert_eq!(open, std::slice::from_ref(&note));
    assert_eq!(open[0].text, "what's the build\nstatus?");
    assert!(open[0].age() < Duration::from_secs(5));
    assert_eq!(mb.note(RID), Some(note));
    assert!(
        mb.post("../escape", "x", me(), Duration::from_secs(1))
            .is_err(),
        "an id that is not one of ours never becomes a path"
    );
}

#[test]
fn notes_come_oldest_first() {
    let cfg = config("order");
    let mb = Mailbox::at(&cfg).unwrap();
    let older = Note {
        request_id: RID2.into(),
        text: "first".into(),
        asked_at: (chrono::Local::now() - chrono::Duration::seconds(30)).to_rfc3339(),
        worker_pid: me(),
        timeout_s: 300,
    };
    mb.post(RID, "second", me(), Duration::from_secs(300))
        .unwrap();
    mb.write_note(&older).unwrap();
    let texts: Vec<String> = mb.open_notes().into_iter().map(|n| n.text).collect();
    assert_eq!(texts, ["first", "second"]);
}

#[test]
fn the_first_reply_wins_and_a_second_is_refused() {
    let cfg = config("first-wins");
    let mb = Mailbox::at(&cfg).unwrap();
    mb.post(RID, "status?", me(), Duration::from_secs(300))
        .unwrap();
    mb.reply(RID, "**Green.** All tests pass.").unwrap();
    assert!(mb.open_notes().is_empty(), "an answered note is not open");
    assert_eq!(mb.reply(RID, "Red."), Err(ReplyRefused::Answered));
    assert_eq!(
        mb.take_reply(RID).as_deref(),
        Some("**Green.** All tests pass.")
    );
    assert!(listing(mb.dir()).is_empty(), "taking the reply clears both");
    assert_eq!(mb.take_reply(RID), None);
}

#[test]
fn a_reply_to_an_expired_or_unknown_note_is_refused_with_the_reason() {
    let cfg = config("refused");
    let mb = Mailbox::at(&cfg).unwrap();
    assert_eq!(mb.reply(RID, "hi"), Err(ReplyRefused::Unknown));
    assert_eq!(mb.reply("../../etc/x", "hi"), Err(ReplyRefused::BadId));
    assert_eq!(
        mb.reply("ambient-00112233445566778899AABB", "hi"),
        Err(ReplyRefused::BadId),
        "upper-case hex is not what request_id makes"
    );
    mb.post(RID, "status?", me(), Duration::from_secs(300))
        .unwrap();
    assert_eq!(mb.reply(RID, "  \n"), Err(ReplyRefused::Empty));
    mb.expire(RID);
    assert_eq!(mb.reply(RID, "late"), Err(ReplyRefused::Expired));
    assert_eq!(
        listing(mb.dir()),
        [format!("{RID}.expired")],
        "the question's text is gone; only the mark is left"
    );
    for refused in [
        ReplyRefused::Expired,
        ReplyRefused::Answered,
        ReplyRefused::Unknown,
    ] {
        assert!(refused.to_string().contains("retry"), "{refused}");
    }
    assert!(ReplyRefused::Expired.to_string().contains("chat"));
}

#[test]
fn a_note_whose_worker_is_gone_is_not_open() {
    let cfg = config("dead");
    let mb = Mailbox::at(&cfg).unwrap();
    mb.post(RID, "status?", dead_pid(), Duration::from_secs(300))
        .unwrap();
    assert!(mb.open_notes().is_empty());
    assert_eq!(mb.reply(RID, "Green."), Err(ReplyRefused::Expired));
    assert!(!mb.dir().join(format!("{RID}.reply.json")).exists());
}

#[test]
fn a_note_older_than_its_timeout_is_not_open() {
    let cfg = config("old");
    let mb = Mailbox::at(&cfg).unwrap();
    let old = Note {
        request_id: RID.into(),
        text: "status?".into(),
        asked_at: (chrono::Local::now() - chrono::Duration::seconds(301)).to_rfc3339(),
        worker_pid: me(),
        timeout_s: 300,
    };
    mb.write_note(&old).unwrap();
    assert!(mb.open_notes().is_empty());
    assert_eq!(mb.reply(RID, "Green."), Err(ReplyRefused::Expired));
    let fresh = Note {
        asked_at: chrono::Local::now().to_rfc3339(),
        ..old
    };
    mb.write_note(&fresh).unwrap();
    assert_eq!(mb.open_notes().len(), 1);
}

#[test]
fn finish_removes_note_and_reply_and_sweep_drops_what_nobody_will_read() {
    let cfg = config("sweep");
    let mb = Mailbox::at(&cfg).unwrap();
    mb.post(RID, "mine", me(), Duration::from_secs(300))
        .unwrap();
    mb.reply(RID, "ok").unwrap();
    mb.finish(RID);
    assert!(listing(mb.dir()).is_empty());

    // A live worker's question, answered but not yet taken: kept.
    mb.post(RID, "mine", me(), Duration::from_secs(300))
        .unwrap();
    mb.reply(RID, "ok").unwrap();
    // A dead worker's question: gone.
    mb.post(RID2, "theirs", dead_pid(), Duration::from_secs(300))
        .unwrap();
    // A fresh mark and an old one, an old staged file and a fresh one.
    let old = SystemTime::now() - Duration::from_secs(2 * 3600);
    let mark = |name: &str, at: Option<SystemTime>| {
        let p = mb.dir().join(name);
        std::fs::write(&p, "").unwrap();
        if let Some(at) = at {
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(at)
                .unwrap();
        }
    };
    mark("ambient-aaaaaaaaaaaaaaaaaaaaaaaa.expired", None);
    mark("ambient-bbbbbbbbbbbbbbbbbbbbbbbb.expired", Some(old));
    mark(".x.json.1.tmp", Some(old));
    mark(".y.json.1.tmp", None);
    mark("ambient-cccccccccccccccccccccccc.reply.json", Some(old));
    mb.sweep();
    assert_eq!(
        listing(mb.dir()),
        [
            ".y.json.1.tmp".to_string(),
            format!("{RID}.json"),
            format!("{RID}.reply.json"),
            "ambient-aaaaaaaaaaaaaaaaaaaaaaaa.expired".into(),
        ]
    );
}

#[test]
fn the_listener_is_gone_when_its_pid_is_dead_or_its_beat_is_stale() {
    let cfg = config("listener");
    assert_eq!(listener(&cfg), None);
    let l = Listener {
        pid: me(),
        id: "a".into(),
        since: chrono::Local::now().to_rfc3339(),
        push: true,
        client: Some("claude-code".into()),
    };
    write_beat(&cfg, &l).unwrap();
    assert_eq!(listener(&cfg), Some(l.clone()));
    write_beat(
        &cfg,
        &Listener {
            pid: dead_pid(),
            ..l.clone()
        },
    )
    .unwrap();
    assert_eq!(listener(&cfg), None, "a dead pid is not listening");
    let stale = json!({"pid": me(), "id": "a", "since": l.since, "push": true, "client": null,
        "updated_at": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() - 11});
    std::fs::write(beat_path(&cfg), stale.to_string()).unwrap();
    assert_eq!(
        listener(&cfg),
        None,
        "a beat older than ten seconds is stale"
    );
}

#[test]
fn only_the_owner_removes_the_beat() {
    let cfg = config("owner");
    let other = Listener {
        pid: 1,
        id: "theirs".into(),
        since: chrono::Local::now().to_rfc3339(),
        push: false,
        client: None,
    };
    write_beat(&cfg, &other).unwrap();
    remove_own_beat(&cfg, "theirs");
    assert_eq!(
        last_beat(&cfg).map(|l| l.pid),
        Some(1),
        "another process's beat is kept"
    );
    let mine = Listener {
        pid: me(),
        id: "mine".into(),
        ..other
    };
    write_beat(&cfg, &mine).unwrap();
    remove_own_beat(&cfg, "not-mine");
    assert!(
        beat_path(&cfg).exists(),
        "another listener in this process is kept"
    );
    remove_own_beat(&cfg, "mine");
    assert!(!beat_path(&cfg).exists());
}

#[test]
fn request_ids_from_the_worker_are_valid_mailbox_ids() {
    for _ in 0..20 {
        let rid = super::super::request_id();
        assert!(valid_id(&rid), "{rid}");
    }
    assert!(!valid_id("ambient-"));
    assert!(!valid_id("ambient-00112233445566778899aab"));
    assert!(!valid_id("ambient-00112233445566778899aabb.json"));
    assert!(!valid_id("other-00112233445566778899aabb"));
}
