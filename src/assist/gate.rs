//! When the assistant may speak: the rules that sit around the jump-in model.
//!
//! The model is asked "should I say something now?" and answers with a yes or
//! no and a confidence. Everything that is not judgement lives here instead,
//! as plain arithmetic a test can pin down: a confidence threshold, a cooldown
//! after each reply, a cap per meeting, and a floor on how often the model is
//! asked at all. Cooldown and cap are checked before the model is called — no
//! point paying for a question whose answer cannot be acted on — and again
//! in [`Gate::judge`], so a verdict is never acted on past either limit.

use std::time::{Duration, Instant};

/// The jump-in model's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub speak: bool,
    /// 0 to 1. Anything outside that range is clamped.
    pub confidence: f32,
    pub reason: String,
}

/// Why the assistant is staying quiet.
#[derive(Debug, Clone, PartialEq)]
pub enum Hold {
    /// It has spoken `max_per_meeting` times already.
    Cap,
    /// It spoke recently; this much of the cooldown is left.
    Cooldown(Duration),
    /// The model was asked moments ago; this long until it may be asked again.
    TooSoon(Duration),
    /// The model said no.
    No,
    /// The model said yes, but not confidently enough.
    BelowThreshold(f32),
}

impl std::fmt::Display for Hold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Hold::Cap => write!(f, "meeting cap reached"),
            Hold::Cooldown(d) => write!(f, "cooling down ({}s left)", d.as_secs()),
            Hold::TooSoon(d) => write!(f, "asking again in {}ms", d.as_millis()),
            Hold::No => write!(f, "nothing to add"),
            Hold::BelowThreshold(c) => write!(f, "confidence {c:.2} below threshold"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Rules {
    pub threshold: f32,
    pub cooldown: Duration,
    pub max_per_meeting: u32,
    /// The least time between two questions to the jump-in model. Bounds the
    /// cost of a meeting where every second brings a new line.
    pub min_interval: Duration,
}

/// The gate for one meeting. A new meeting gets a new gate.
#[derive(Debug)]
pub struct Gate {
    rules: Rules,
    spoken: u32,
    last_spoke: Option<Instant>,
    last_asked: Option<Instant>,
}

impl Gate {
    pub fn new(rules: Rules) -> Self {
        Self {
            rules,
            spoken: 0,
            last_spoke: None,
            last_asked: None,
        }
    }

    pub fn spoken(&self) -> u32 {
        self.spoken
    }

    /// Whether the assistant could speak at all right now, regardless of
    /// what the model would say.
    fn open(&self, now: Instant) -> Result<(), Hold> {
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

    /// Before calling the jump-in model: is there any point?
    pub fn may_ask(&self, now: Instant) -> Result<(), Hold> {
        self.open(now)?;
        if let Some(t) = self.last_asked {
            let since = now.saturating_duration_since(t);
            if since < self.rules.min_interval {
                return Err(Hold::TooSoon(self.rules.min_interval - since));
            }
        }
        Ok(())
    }

    /// The model was asked at `now`.
    pub fn asked(&mut self, now: Instant) {
        self.last_asked = Some(now);
    }

    /// After the model answered: speak, or why not.
    pub fn judge(&self, verdict: &Verdict, now: Instant) -> Result<(), Hold> {
        if !verdict.speak {
            return Err(Hold::No);
        }
        let confidence = verdict.confidence.clamp(0.0, 1.0);
        if confidence < self.rules.threshold {
            return Err(Hold::BelowThreshold(confidence));
        }
        self.open(now)
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
            threshold: 0.75,
            cooldown: Duration::from_secs(60),
            max_per_meeting: 2,
            min_interval: Duration::from_secs(5),
        }
    }

    fn yes(confidence: f32) -> Verdict {
        Verdict {
            speak: true,
            confidence,
            reason: "asked directly".into(),
        }
    }

    #[test]
    fn a_confident_yes_on_a_fresh_meeting_speaks() {
        let g = Gate::new(rules());
        let t = Instant::now();
        assert_eq!(g.may_ask(t), Ok(()));
        assert_eq!(g.judge(&yes(0.9), t), Ok(()));
    }

    #[test]
    fn the_threshold_is_inclusive_and_a_no_is_a_no_at_any_confidence() {
        let g = Gate::new(rules());
        let t = Instant::now();
        assert_eq!(g.judge(&yes(0.75), t), Ok(()));
        assert_eq!(g.judge(&yes(0.74), t), Err(Hold::BelowThreshold(0.74)));
        let no = Verdict {
            speak: false,
            confidence: 1.0,
            reason: "small talk".into(),
        };
        assert_eq!(g.judge(&no, t), Err(Hold::No));
    }

    #[test]
    fn an_out_of_range_confidence_is_clamped_not_trusted() {
        let g = Gate::new(rules());
        let t = Instant::now();
        assert_eq!(g.judge(&yes(7.0), t), Ok(()));
        assert_eq!(g.judge(&yes(-1.0), t), Err(Hold::BelowThreshold(0.0)));
    }

    #[test]
    fn after_speaking_it_cools_down_before_asking_or_speaking_again() {
        let mut g = Gate::new(rules());
        let t = Instant::now();
        g.spoke(t);
        let later = t + Duration::from_secs(20);
        assert_eq!(
            g.may_ask(later),
            Err(Hold::Cooldown(Duration::from_secs(40)))
        );
        assert_eq!(
            g.judge(&yes(0.99), later),
            Err(Hold::Cooldown(Duration::from_secs(40)))
        );
        let after = t + Duration::from_secs(60);
        assert_eq!(g.may_ask(after), Ok(()));
        assert_eq!(g.judge(&yes(0.99), after), Ok(()));
    }

    #[test]
    fn the_cooldown_is_rechecked_after_the_model_answers() {
        // A slow reply-model call can finish inside a cooldown that started
        // while the jump-in question was in flight; judge must still refuse.
        let mut g = Gate::new(rules());
        let t = Instant::now();
        assert_eq!(g.may_ask(t), Ok(()));
        g.spoke(t + Duration::from_secs(1));
        assert!(matches!(
            g.judge(&yes(0.9), t + Duration::from_secs(2)),
            Err(Hold::Cooldown(_))
        ));
    }

    #[test]
    fn the_cap_holds_for_the_rest_of_the_meeting() {
        let mut g = Gate::new(rules());
        let t = Instant::now();
        g.spoke(t);
        g.spoke(t + Duration::from_secs(61));
        assert_eq!(g.spoken(), 2);
        let much_later = t + Duration::from_secs(3600);
        assert_eq!(g.may_ask(much_later), Err(Hold::Cap));
        assert_eq!(g.judge(&yes(1.0), much_later), Err(Hold::Cap));
        // A new meeting starts with a new gate.
        assert_eq!(Gate::new(rules()).may_ask(much_later), Ok(()));
    }

    #[test]
    fn a_cap_of_zero_never_speaks() {
        let g = Gate::new(Rules {
            max_per_meeting: 0,
            ..rules()
        });
        assert_eq!(g.may_ask(Instant::now()), Err(Hold::Cap));
    }

    #[test]
    fn the_model_is_not_asked_more_often_than_the_minimum_interval() {
        let mut g = Gate::new(rules());
        let t = Instant::now();
        g.asked(t);
        assert_eq!(
            g.may_ask(t + Duration::from_secs(2)),
            Err(Hold::TooSoon(Duration::from_secs(3)))
        );
        assert_eq!(g.may_ask(t + Duration::from_secs(5)), Ok(()));
        // Asking does not count against the cap; only speaking does.
        assert_eq!(g.spoken(), 0);
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
