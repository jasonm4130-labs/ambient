use super::*;
use std::path::Path;

/// A config file of its own per test, in a fresh folder, so the mailbox and
/// heartbeat beside it are the test's alone.
fn config(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ambient-listen-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("config.json")
}

const RID: &str = "ambient-00112233445566778899aabb";

/// A desk whose output can be read back.
fn desk(cfg: &Path, tick: Duration) -> (Desk, Arc<Mutex<Vec<u8>>>) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let out: Out = buf.clone();
    (Desk::new(cfg.to_path_buf(), out).with_tick(tick), buf)
}

/// How many channel notifications a desk has written.
fn pushed(buf: &Mutex<Vec<u8>>) -> usize {
    String::from_utf8(lock(buf).clone())
        .unwrap()
        .lines()
        .filter(|l| l.contains("notifications/claude/channel"))
        .count()
}

fn id(d: &Desk) -> String {
    lock(&d.shared).id.clone()
}

#[test]
fn a_listener_that_was_replaced_pushes_nothing_even_before_its_next_heartbeat() {
    let cfg = config("handover");
    // A heartbeat every 8 ticks of 50 ms: the first desk's next one is
    // 400 ms away when the second takes over.
    let tick = Duration::from_millis(50);
    let (mut first, first_out) = desk(&cfg, tick);
    first.listen(true).unwrap();
    std::thread::sleep(Duration::from_millis(60));
    let (mut second, second_out) = desk(&cfg, tick);
    second.listen(true).unwrap();
    Mailbox::at(&cfg)
        .unwrap()
        .post(
            RID,
            "who hears this?",
            std::process::id(),
            Duration::from_secs(300),
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(pushed(&first_out), 0, "the replaced listener stays quiet");
    assert_eq!(pushed(&second_out), 1, "the new listener hears it once");
    assert!(!first.listening());
    assert!(second.listening());
}

#[test]
fn a_new_listener_is_never_overwritten_by_the_one_it_replaced() {
    let cfg = config("overwrite");
    for round in 0..40 {
        // A tick of zero keeps the first desk refreshing its heartbeat as
        // fast as it can, so any gap between its check and its write shows.
        let (mut first, _) = desk(&cfg, Duration::ZERO);
        first.listen(true).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let (mut second, _) = desk(&cfg, Duration::from_millis(1));
        second.listen(true).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(
            mailbox::last_beat(&cfg).map(|l| l.id),
            Some(id(&second)),
            "round {round}: the heartbeat names the last listener"
        );
        assert!(second.listening(), "round {round}");
        assert!(!first.listening(), "round {round}");
        second.stop();
        first.stop();
    }
}

#[test]
fn after_stop_listening_the_heartbeat_stays_gone() {
    let cfg = config("stop");
    for round in 0..60 {
        let (mut d, _) = desk(&cfg, Duration::ZERO);
        d.listen(true).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        d.stop();
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(
            mailbox::last_beat(&cfg),
            None,
            "round {round}: no heartbeat names a session that stopped"
        );
    }
}
