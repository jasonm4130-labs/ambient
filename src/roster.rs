//! The people you record with, so a name is typed once rather than once per
//! recording.
//!
//! This is a list of names and nothing else. It deliberately stores no
//! voiceprints: an embedding kept to recognise someone later is biometric data
//! under Article 9, which is a different compliance regime from a text file of
//! names. The roster removes the retyping, not the choosing — assignment stays
//! a human decision, made through `session::name_speaker`.

use std::path::{Path, PathBuf};

use anyhow::Result;

/// Beside the config, for the same reason: the sessions folder is itself a
/// setting, so nothing that must be readable before settings load can live in
/// it.
pub fn path() -> PathBuf {
    if let Ok(p) = std::env::var("AMBIENT_ROSTER") {
        return PathBuf::from(p);
    }
    crate::config::path().with_file_name("roster.json")
}

/// Never fails, for the same reason the config does not: a malformed list of
/// names must not be able to stop a recording.
pub fn load() -> Vec<String> {
    load_from(&path())
}

pub fn load_from(p: &Path) -> Vec<String> {
    let text = match std::fs::read_to_string(p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            eprintln!("  WARNING: {} could not be read ({e})", p.display());
            return Vec::new();
        }
    };
    match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "  WARNING: {} is not a valid roster ({e}) — treating it as empty",
                p.display()
            );
            Vec::new()
        }
    }
}

pub fn save(names: &[String]) -> Result<()> {
    save_to(&path(), names)
}

pub fn save_to(p: &Path, names: &[String]) -> Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(p, serde_json::to_string_pretty(names)?)?;
    Ok(())
}

/// Adding a name already present is a no-op rather than an error: the caller is
/// usually a person typing, and refusing them a name they already have would be
/// pedantry. Returns whether the list changed.
pub fn add(names: &mut Vec<String>, who: &str) -> bool {
    let who = who.trim();
    if who.is_empty() || names.iter().any(|n| n.eq_ignore_ascii_case(who)) {
        return false;
    }
    names.push(who.to_string());
    names.sort_by_key(|n| n.to_lowercase());
    true
}

/// Removing someone from the roster does not unname them in past recordings —
/// those names live in each session's `edits.jsonl` and stay put. This only
/// stops offering them next time.
pub fn remove(names: &mut Vec<String>, who: &str) -> bool {
    let before = names.len();
    names.retain(|n| !n.eq_ignore_ascii_case(who.trim()));
    names.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("ambient-roster-{name}-{}.json", std::process::id()));
        std::fs::remove_file(&p).ok();
        p
    }

    #[test]
    fn a_missing_roster_is_empty_rather_than_an_error() {
        assert!(load_from(&temp("missing")).is_empty());
    }

    #[test]
    fn a_corrupt_roster_does_not_stop_the_app() {
        let p = temp("corrupt");
        std::fs::write(&p, "not json at all").unwrap();
        assert!(load_from(&p).is_empty());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn it_round_trips_through_the_file() {
        let p = temp("roundtrip");
        let mut names = Vec::new();
        add(&mut names, "Priya");
        add(&mut names, "Marcus");
        save_to(&p, &names).unwrap();
        assert_eq!(load_from(&p), ["Marcus", "Priya"]);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn the_same_person_is_not_added_twice() {
        let mut names = vec!["Priya".to_string()];
        assert!(!add(&mut names, "priya"));
        assert!(!add(&mut names, "  Priya  "));
        assert_eq!(names, ["Priya"]);
    }

    #[test]
    fn a_blank_name_is_refused() {
        let mut names = Vec::new();
        assert!(!add(&mut names, "   "));
        assert!(names.is_empty());
    }

    #[test]
    fn removing_someone_reports_whether_it_did_anything() {
        let mut names = vec!["Priya".to_string()];
        assert!(remove(&mut names, "PRIYA"));
        assert!(names.is_empty());
        assert!(!remove(&mut names, "Priya"));
    }
}
