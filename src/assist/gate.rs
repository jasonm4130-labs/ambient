//! When the assistant may speak: the limits that sit around the model.
//!
//! Whether something is worth saying is the model's call, made in the
//! user's own Claude Code session. How often it may act on that call is not:
//! a cooldown after each utterance and a cap per meeting are plain arithmetic
//! here, enforced by `speak` whatever the model decides, so a model that loses
//! the thread cannot talk over a meeting.

use std::time::{Duration, Instant};

/// Why the assistant may not speak right now.
#[derive(Debug, Clone, PartialEq)]
pub enum Hold {
    /// It has spoken `max_per_meeting` times already.
    Cap,
    /// It spoke recently; this much of the cooldown is left.
    Cooldown(Duration),
}

impl std::fmt::Display for Hold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Hold::Cap => write!(f, "it has spoken as often as one meeting allows"),
            Hold::Cooldown(d) => write!(f, "cooling down ({}s left)", d.as_secs().max(1)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Rules {
    pub cooldown: Duration,
    pub max_per_meeting: u32,
}

/// The gate for one meeting. A new meeting gets a new gate.
#[derive(Debug)]
pub struct Gate {
    rules: Rules,
    spoken: u32,
    last_spoke: Option<Instant>,
}

impl Gate {
    pub fn new(rules: Rules) -> Self {
        Self {
            rules,
            spoken: 0,
            last_spoke: None,
        }
    }

    pub fn spoken(&self) -> u32 {
        self.spoken
    }

    pub fn rules(&self) -> &Rules {
        &self.rules
    }

    /// Whether the assistant may speak at `now`.
    pub fn may_speak(&self, now: Instant) -> Result<(), Hold> {
        if self.spoken >= self.rules.max_per_meeting {
            return Err(Hold::Cap);
        }
        if let Some(t) = self.last_spoke {
            let since = now.saturating_duration_since(t);
            if since < self.rules.cooldown {
                return Err(Hold::Cooldown(self.rules.cooldown - since));
            }
        }
        Ok(())
    }

    /// The assistant spoke at `now`.
    pub fn spoke(&mut self, now: Instant) {
        self.spoken += 1;
        self.last_spoke = Some(now);
    }
}

/// Whether a transcript line is the assistant hearing itself.
///
/// The assistant's voice comes out of the speakers and back in through the
/// microphone, so its own reply turns up in the transcript a few seconds
/// later. Treated as a person, that line would invite a reply to itself. A
/// line counts as an echo when most of its words appear in something the
/// assistant said recently; lines under three words are too short to tell.
pub fn is_echo(line: &str, recent_replies: &[&str]) -> bool {
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric() && c != '\'')
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let heard = words(line);
    if heard.len() < 3 {
        return false;
    }
    recent_replies.iter().any(|reply| {
        let said: std::collections::HashSet<String> = words(reply).into_iter().collect();
        let shared = heard.iter().filter(|w| said.contains(*w)).count();
        shared * 10 >= heard.len() * 6
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Rules {
        Rules {
            cooldown: Duration::from_secs(60),
            max_per_meeting: 2,
        }
    }

    #[test]
    fn a_fresh_meeting_may_speak() {
        assert_eq!(Gate::new(rules()).may_speak(Instant::now()), Ok(()));
    }

    #[test]
    fn after_speaking_it_cools_down() {
        let mut g = Gate::new(rules());
        let t = Instant::now();
        g.spoke(t);
        assert_eq!(
            g.may_speak(t + Duration::from_secs(20)),
            Err(Hold::Cooldown(Duration::from_secs(40)))
        );
        assert_eq!(g.may_speak(t + Duration::from_secs(60)), Ok(()));
    }

    #[test]
    fn the_cap_holds_for_the_rest_of_the_meeting() {
        let mut g = Gate::new(rules());
        let t = Instant::now();
        g.spoke(t);
        g.spoke(t + Duration::from_secs(61));
        assert_eq!(g.spoken(), 2);
        let much_later = t + Duration::from_secs(3600);
        assert_eq!(g.may_speak(much_later), Err(Hold::Cap));
        // A new meeting starts with a new gate.
        assert_eq!(Gate::new(rules()).may_speak(much_later), Ok(()));
    }

    #[test]
    fn a_cap_of_zero_never_speaks() {
        let g = Gate::new(Rules {
            max_per_meeting: 0,
            ..rules()
        });
        assert_eq!(g.may_speak(Instant::now()), Err(Hold::Cap));
    }

    #[test]
    fn its_own_reply_heard_back_is_an_echo() {
        let said = ["That ticket was closed on Tuesday, so you can drop it."];
        assert!(is_echo("that ticket was closed on tuesday", &said));
        assert!(is_echo(
            "ticket was closed Tuesday so we can drop it",
            &said
        ));
        assert!(!is_echo("was the deploy ticket closed or not", &said));
        assert!(!is_echo("closed Tuesday", &said), "too short to tell");
        assert!(!is_echo("that ticket was closed on tuesday", &[]));
    }
}
