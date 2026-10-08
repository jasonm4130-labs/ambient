//! The two model calls: "should I speak?" and "what do I say?".
//!
//! Both go to an OpenAI-compatible chat endpoint — OpenRouter, directly or
//! through a Cloudflare AI Gateway — so the model behind each is a setting.
//! The prompts ask for a small JSON object, and the parsers accept that
//! object wherever it appears in the reply, because a model that wraps its
//! JSON in a sentence or a code fence has still answered.

use super::gate::Verdict;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// One chat completion, reduced to what the assistant needs.
#[derive(Debug, Clone)]
pub struct Request {
    pub model: String,
    pub system: String,
    pub user: String,
    pub max_tokens: u32,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct Completion {
    pub text: String,
    pub latency: Duration,
    /// USD, when the endpoint reports it.
    pub cost: Option<f64>,
}

/// Anything that answers a [`Request`]. The real one is [`OpenRouter`]; tests
/// script their own.
pub trait Model {
    fn complete(&self, request: &Request) -> Result<Completion>;
}

/// OpenRouter's chat completions endpoint.
pub struct OpenRouter {
    agent: ureq::Agent,
    base_url: String,
    key: String,
    /// `cf-aig-authorization`, for a Cloudflare AI Gateway that requires it.
    gateway_token: Option<String>,
    zdr: bool,
}

impl OpenRouter {
    /// The key comes from the environment only — `OPENROUTER_API_KEY`, which
    /// `op run --env-file .env.assistant.op` fills from 1Password — and never from a
    /// file Ambient writes.
    pub fn from_env(base_url: &str, zdr: bool) -> Result<Self> {
        let key = std::env::var("OPENROUTER_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "OPENROUTER_API_KEY is not set. Run the assistant under 1Password: \
                     op run --env-file .env.assistant.op -- ambient assist"
                )
            })?;
        let gateway_token = std::env::var("CF_AIG_TOKEN")
            .ok()
            .filter(|k| !k.trim().is_empty());
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            // The system keychain's roots, so a managed Mac's own roots work.
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Self {
            agent,
            base_url: base_url.trim_end_matches('/').to_string(),
            key,
            gateway_token,
            zdr,
        })
    }
}

/// The request body, separate from the transport so a test can read it.
pub fn body(request: &Request, zdr: bool) -> Value {
    let mut body = json!({
        "model": request.model,
        "messages": [
            {"role": "system", "content": request.system},
            {"role": "user", "content": request.user},
        ],
        "max_tokens": request.max_tokens,
        "temperature": 0.2,
        "usage": {"include": true},
    });
    if zdr {
        // Route only to providers with a zero-data-retention policy, and to
        // none that train on prompts. With no such provider the call fails
        // rather than quietly falling back to one that retains.
        body["provider"] = json!({"zdr": true, "data_collection": "deny"});
    }
    body
}

impl Model for OpenRouter {
    fn complete(&self, request: &Request) -> Result<Completion> {
        let started = Instant::now();
        let url = format!("{}/chat/completions", self.base_url);
        let mut call = self
            .agent
            .post(&url)
            .config()
            .timeout_global(Some(request.timeout))
            .build()
            .header("Authorization", &format!("Bearer {}", self.key))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://github.com/jasonm4130-labs/ambient")
            .header("X-Title", "Ambient live assistant");
        if let Some(t) = &self.gateway_token {
            call = call.header("cf-aig-authorization", &format!("Bearer {t}"));
        }
        let mut response = call
            .send(body(request, self.zdr).to_string())
            .with_context(|| format!("calling {}", request.model))?;
        let status = response.status();
        let text = response
            .body_mut()
            .read_to_string()
            .context("reading the model's answer")?;
        if !status.is_success() {
            // The body says why (no ZDR provider, bad key, out of credit); the
            // key itself never appears in it.
            bail!("{} answered {status}: {}", request.model, clip(&text, 400));
        }
        let v: Value = serde_json::from_str(&text).context("the model's answer is not JSON")?;
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("no message in {}", clip(&text, 400)))?;
        Ok(Completion {
            text: content.to_string(),
            latency: started.elapsed(),
            cost: v["usage"]["cost"].as_f64(),
        })
    }
}

fn clip(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

/// The emotions the reply may carry. The voice helper maps each to a delivery
/// instruction (Qwen3-TTS) or a tag (Supertonic); anything else reads as
/// neutral.
pub const EMOTIONS: [&str; 6] = [
    "neutral",
    "warm",
    "amused",
    "excited",
    "apologetic",
    "concerned",
];

/// The jump-in question. Written to make "no" the easy answer: a meeting
/// assistant that talks too much is switched off, one that is quiet until it
/// matters is kept.
pub fn jump_in_system(name: &str) -> String {
    format!(
        "You decide whether {name}, an AI assistant listening to a live meeting, should speak \
up right now. Everyone in the meeting has been told {name} is listening and may speak.\n\n\
Say yes only when at least one of these is true of the most recent lines:\n\
- someone addresses {name} by name, or asks the assistant or \"the AI\" something;\n\
- a factual question was asked, nobody has answered it, and a short factual answer would help;\n\
- someone states something clearly and checkably wrong that the group is about to act on.\n\n\
Say no to small talk, opinions, brainstorming, questions people are already answering, \
anything rhetorical, and anything {name} said itself. When unsure, say no.\n\n\
Reply with only a JSON object: {{\"speak\": true|false, \"confidence\": 0.0-1.0, \
\"reason\": \"<ten words or fewer>\"}}"
    )
}

/// The reply. Spoken aloud, so short, plain and with no formatting.
pub fn reply_system(name: &str) -> String {
    format!(
        "You are {name}, an AI assistant taking part in a live meeting by voice. Your words \
will be spoken aloud by a text-to-speech voice, straight into the meeting.\n\n\
Answer the moment you were brought in for, in one to three short spoken sentences. No \
lists, no markdown, no emoji, no preamble such as \"Great question\". If you do not know, \
say so briefly. Do not repeat what was said.\n\n\
Reply with only a JSON object: {{\"text\": \"<what to say>\", \"emotion\": \"<one of: {}>\"}}",
        EMOTIONS.join(", ")
    )
}

/// The first `{...}` object in a reply that parses, wherever it sits.
fn json_object(text: &str) -> Option<Value> {
    let bytes = text.as_bytes();
    for (start, _) in text.match_indices('{') {
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for (i, &b) in bytes.iter().enumerate().skip(start) {
            if in_string {
                match (escaped, b) {
                    (true, _) => escaped = false,
                    (false, b'\\') => escaped = true,
                    (false, b'"') => in_string = false,
                    _ => {}
                }
                continue;
            }
            match b {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        if let Ok(v) = serde_json::from_str::<Value>(&text[start..=i]) {
                            if v.is_object() {
                                return Some(v);
                            }
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// The jump-in model's answer. Anything unreadable is a no: silence is the
/// safe failure for a voice in a meeting.
pub fn parse_verdict(text: &str) -> Verdict {
    let Some(v) = json_object(text) else {
        return Verdict {
            speak: false,
            confidence: 0.0,
            reason: format!("unreadable answer: {}", clip(text, 80)),
        };
    };
    Verdict {
        speak: v["speak"].as_bool().unwrap_or(false),
        confidence: v["confidence"].as_f64().unwrap_or(0.0) as f32,
        reason: v["reason"].as_str().unwrap_or("").to_string(),
    }
}

/// What to say, and how.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub text: String,
    pub emotion: String,
}

/// The reply model's answer. A reply that is not the JSON asked for is used
/// as plain text if it is short enough to have been meant as speech.
pub fn parse_reply(text: &str) -> Option<Reply> {
    let (said, emotion) = match json_object(text) {
        Some(v) => (
            v["text"].as_str().unwrap_or("").to_string(),
            v["emotion"].as_str().unwrap_or("neutral").to_string(),
        ),
        None if text.len() <= 400 && !text.contains('{') => (text.to_string(), "neutral".into()),
        None => return None,
    };
    let said = said.split_whitespace().collect::<Vec<_>>().join(" ");
    if said.is_empty() {
        return None;
    }
    let emotion = if EMOTIONS.contains(&emotion.as_str()) {
        emotion
    } else {
        "neutral".into()
    };
    Some(Reply {
        text: said,
        emotion,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verdict_is_read_from_bare_fenced_or_wrapped_json() {
        let bare =
            parse_verdict(r#"{"speak": true, "confidence": 0.9, "reason": "asked by name"}"#);
        assert!(bare.speak);
        assert_eq!(bare.confidence, 0.9);
        assert_eq!(bare.reason, "asked by name");
        let fenced = parse_verdict(
            "```json\n{\"speak\": false, \"confidence\": 0.2, \"reason\": \"x\"}\n```",
        );
        assert!(!fenced.speak);
        let wrapped = parse_verdict(
            r#"Sure. {"speak": true, "confidence": 0.8, "reason": "a {brace} in text"} done"#,
        );
        assert!(wrapped.speak);
        assert_eq!(wrapped.reason, "a {brace} in text");
    }

    #[test]
    fn an_unreadable_verdict_is_a_no() {
        for text in ["", "yes", "{not json", r#"{"speak": "yes"}"#] {
            let v = parse_verdict(text);
            assert!(!v.speak, "{text:?} -> {v:?}");
        }
    }

    #[test]
    fn a_reply_keeps_a_known_emotion_and_flattens_whitespace() {
        let r = parse_reply(r#"{"text": "It closed\non Tuesday.", "emotion": "amused"}"#).unwrap();
        assert_eq!(r.text, "It closed on Tuesday.");
        assert_eq!(r.emotion, "amused");
        let r = parse_reply(r#"{"text": "Sure.", "emotion": "furious"}"#).unwrap();
        assert_eq!(r.emotion, "neutral", "an unknown emotion is neutral");
    }

    #[test]
    fn plain_speech_is_accepted_but_empty_or_garbled_replies_are_not() {
        assert_eq!(
            parse_reply("It closed on Tuesday.").unwrap().text,
            "It closed on Tuesday."
        );
        assert!(parse_reply(r#"{"text": "  "}"#).is_none());
        assert!(parse_reply(r#"{"text": "x""#).is_none());
        assert!(
            parse_reply(&"word ".repeat(200)).is_none(),
            "too long to be speech"
        );
    }

    #[test]
    fn zdr_routing_is_requested_only_when_on() {
        let r = Request {
            model: "anthropic/claude-haiku-5.5".into(),
            system: "s".into(),
            user: "u".into(),
            max_tokens: 50,
            timeout: Duration::from_secs(5),
        };
        let on = body(&r, true);
        assert_eq!(on["provider"]["zdr"], true);
        assert_eq!(on["provider"]["data_collection"], "deny");
        assert_eq!(on["model"], "anthropic/claude-haiku-5.5");
        assert_eq!(on["messages"][0]["role"], "system");
        assert!(body(&r, false).get("provider").is_none());
    }

    #[test]
    fn the_prompts_name_the_assistant_and_the_emotions() {
        assert!(jump_in_system("Juno").contains("Juno"));
        let reply = reply_system("Juno");
        for e in EMOTIONS {
            assert!(reply.contains(e), "{e}");
        }
    }

    #[test]
    fn a_missing_key_says_how_to_supply_it() {
        // Only meaningful when the test process has no key of its own.
        if std::env::var("OPENROUTER_API_KEY").is_ok() {
            return;
        }
        let e = OpenRouter::from_env("https://openrouter.ai/api/v1", true)
            .err()
            .unwrap();
        assert!(
            e.to_string()
                .contains("op run --env-file .env.assistant.op"),
            "{e}"
        );
    }
}
