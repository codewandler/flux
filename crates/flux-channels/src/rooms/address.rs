//! The **address rule** (D-207) — whether a room message is spoken *to flux*.
//!
//! This is the difference between a meeting participant and a bot that talks over everyone. flux
//! hears every line said in a room and is the addressee of almost none of it; without a rule it
//! answers every sentence two humans say to each other, and two agents answering each other's
//! mentions never stop. The 2026-07-30 spike is the live evidence: the bot replied to every inbound
//! line and read as spam within three messages.
//!
//! ## What counts as being addressed
//!
//! - **A private message always does.** Nobody whispers to the agent and does not mean it, so the
//!   configured rule governs *public* text only. This is why [`MessageScope`] had to survive the trip
//!   through the port (D-204).
//! - **Public text** counts when the configured rule says so: our room-visible name was mentioned
//!   ([`AddressRule::MENTION`]), or a configured wake phrase opened the line
//!   ([`AddressRule::wake`]).
//!
//! ## What can never count
//!
//! **Another automated participant's plain text is never an address**, whatever the rule says and
//! whatever scope it arrived on. Two agents that each answer the other's mention are an unbounded
//! exchange started by one human sentence, and the way to bound that is to refuse the shape rather
//! than to rate-limit it afterwards. The one thing that *does* get through from an agent is a
//! **structured A2A envelope** — see [`is_a2a_envelope`], which is the seam D-212 lands on.
//!
//! That refusal is only as good as the backend's `OccupantKind`, and XMPP presence carries no
//! human-or-bot signal, so a real MUC reports [`OccupantKind::Unknown`] for everyone (D-205). The
//! rule therefore covers the case flux *can* see, and the [reply budget](super::ReplyBudget) covers
//! the case it cannot — belt and braces, deliberately, because only one of the two is structural.
//!
//! ## Identity
//!
//! Mention matching compares text against **our own** configured nick — the name a person types when
//! they mean us. It never identifies a *speaker* by nick: occupants may share one
//! ([`Occupant::nick`](super::Occupant::nick)), and [`OccupantId`](super::OccupantId) is the stable
//! id for that.

use std::fmt;

use super::{MessageScope, OccupantKind};

/// Why a room message counted as spoken to flux — or why it did not. Returned rather than a bare
/// `bool` so the driver can log *which* rule kept it quiet; a room that mysteriously says nothing is
/// the hardest kind of misconfiguration to diagnose from the outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addressing {
    /// A private message — directed at us by construction.
    Whisper,
    /// Public text naming us.
    Mention,
    /// Public text carrying a configured wake phrase.
    Wake,
    /// The rule accepts every public line ([`AddressRule::always`]).
    Always,
    /// Public text that named nobody the rule accepts.
    NotAddressed,
    /// Another automated participant's plain text. Never auto-answered, at any scope — only a
    /// structured A2A envelope is (D-212).
    AgentPlainText,
}

impl Addressing {
    /// Whether the agent should take this message as a turn.
    pub fn should_answer(self) -> bool {
        matches!(
            self,
            Addressing::Whisper | Addressing::Mention | Addressing::Wake | Addressing::Always
        )
    }

    /// A short reason, for the log line that explains a silence.
    pub fn reason(self) -> &'static str {
        match self {
            Addressing::Whisper => "a private message",
            Addressing::Mention => "our nick was mentioned",
            Addressing::Wake => "a wake phrase",
            Addressing::Always => "the rule answers everything",
            Addressing::NotAddressed => "not addressed to us",
            Addressing::AgentPlainText => "another agent's plain text",
        }
    }
}

/// When public room text is aimed at flux.
///
/// Parsed from the `address_rule` channel setting: a comma-separated list of signals, each one of
///
/// | token | meaning |
/// |---|---|
/// | `mention` | public text naming us (the default) |
/// | `wake: <phrase>` | public text containing `<phrase>` |
/// | `always` | every public line — the pre-D-207 behaviour, and opt-in for a reason |
/// | `never` | no public line; whispers only |
///
/// `always` and `never` are whole rules and cannot be combined with anything else. An unrecognized
/// token is a **load error**: this is operator configuration, and a rule that silently degraded to
/// "answer everything" because of a typo is the exact failure this type exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressRule {
    mention: bool,
    always: bool,
    wake: Vec<String>,
}

/// The rule a room channel uses when the declaration does not say: answer public text that names us,
/// and every whisper. Deliberately not `always` — an agent that answers everything is the bug D-207
/// exists to fix, so it must be something an operator asks for in writing.
pub const DEFAULT_ADDRESS_RULE: &str = "mention";

impl Default for AddressRule {
    fn default() -> Self {
        Self::mention()
    }
}

impl AddressRule {
    /// The `mention` token, spelled once.
    pub const MENTION: &'static str = "mention";

    /// Public text naming us addresses us — the default.
    pub fn mention() -> Self {
        Self {
            mention: true,
            always: false,
            wake: Vec::new(),
        }
    }

    /// Every public line addresses us. The pre-D-207 behaviour, kept reachable because a
    /// single-purpose bot room is a legitimate deployment — and opt-in because a meeting room is not.
    pub fn always() -> Self {
        Self {
            mention: false,
            always: true,
            wake: Vec::new(),
        }
    }

    /// No public line addresses us; only a whisper does.
    pub fn never() -> Self {
        Self {
            mention: false,
            always: false,
            wake: Vec::new(),
        }
    }

    /// Add a wake phrase. Matched case-insensitively anywhere in the line, at word boundaries — so
    /// `wake: ok flux` fires on "ok flux, when is standup?" and not inside another word.
    pub fn wake(mut self, phrase: impl Into<String>) -> Self {
        let phrase = phrase.into().trim().to_lowercase();
        if !phrase.is_empty() {
            self.wake.push(phrase);
        }
        self
    }

    /// Parse the `address_rule` setting. See the type docs for the vocabulary.
    pub fn parse(spec: &str) -> Result<Self, AddressRuleError> {
        let mut rule = Self::never();
        let mut whole: Option<&'static str> = None;
        let mut tokens = 0usize;

        for raw in spec.split(',') {
            let token = raw.trim();
            if token.is_empty() {
                continue;
            }
            tokens += 1;
            let lower = token.to_lowercase();
            match lower.split_once(':') {
                Some((head, phrase)) if head.trim() == "wake" => {
                    let phrase = phrase.trim();
                    if phrase.is_empty() {
                        return Err(AddressRuleError::EmptyWakePhrase);
                    }
                    rule = rule.wake(phrase);
                }
                _ => match lower.as_str() {
                    "mention" => rule.mention = true,
                    "always" => {
                        rule.always = true;
                        whole = Some("always");
                    }
                    "never" => whole = Some("never"),
                    _ => return Err(AddressRuleError::UnknownToken(token.to_string())),
                },
            }
        }

        if tokens == 0 {
            return Err(AddressRuleError::Empty);
        }
        if let Some(whole) = whole {
            if tokens > 1 {
                return Err(AddressRuleError::NotCombinable(whole));
            }
            return Ok(if whole == "always" {
                Self::always()
            } else {
                Self::never()
            });
        }
        Ok(rule)
    }

    /// Classify one inbound message. `nick` is **our** room-visible name; `kind` is what the backend
    /// knows about the speaker.
    pub fn classify(
        &self,
        nick: &str,
        kind: OccupantKind,
        scope: MessageScope,
        text: &str,
    ) -> Addressing {
        // First, and before the scope: another automated participant's plain text is never a turn,
        // however it arrived. A whisper does not make an agent's chatter addressed — two agents
        // whispering is the same runaway as two agents talking.
        if kind == OccupantKind::Agent && !is_a2a_envelope(text) {
            return Addressing::AgentPlainText;
        }
        if scope == MessageScope::Private {
            return Addressing::Whisper;
        }
        if self.always {
            return Addressing::Always;
        }
        let lower = text.to_lowercase();
        if self.mention && !nick.is_empty() && contains_token(&lower, &nick.to_lowercase()) {
            return Addressing::Mention;
        }
        if self.wake.iter().any(|p| contains_token(&lower, p)) {
            return Addressing::Wake;
        }
        Addressing::NotAddressed
    }
}

impl fmt::Display for AddressRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.always {
            return f.write_str("always");
        }
        let mut parts: Vec<String> = Vec::new();
        if self.mention {
            parts.push(Self::MENTION.to_string());
        }
        parts.extend(self.wake.iter().map(|p| format!("wake: {p}")));
        if parts.is_empty() {
            return f.write_str("never");
        }
        f.write_str(&parts.join(", "))
    }
}

/// Why an `address_rule` could not be parsed. A load error rather than a warning: see
/// [`AddressRule`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressRuleError {
    /// The setting was present but said nothing.
    Empty,
    /// A token outside the vocabulary.
    UnknownToken(String),
    /// `wake:` with nothing after it — a phrase that matches every line.
    EmptyWakePhrase,
    /// `always` or `never` combined with another signal.
    NotCombinable(&'static str),
}

impl fmt::Display for AddressRuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str(
                "empty `address_rule` (known: mention, `wake: <phrase>`, always, never)",
            ),
            Self::UnknownToken(t) => write!(
                f,
                "unknown `address_rule` token `{t}` (known: mention, `wake: <phrase>`, always, never)"
            ),
            Self::EmptyWakePhrase => {
                f.write_str("`address_rule` wake phrase is empty — that would match every line")
            }
            Self::NotCombinable(w) => write!(
                f,
                "`address_rule` `{w}` is a whole rule and cannot be combined with another signal"
            ),
        }
    }
}

impl std::error::Error for AddressRuleError {}

/// Whether room text is a **structured A2A envelope** rather than something an agent said in prose.
///
/// This is the seam D-212 lands on, and it is deliberately narrow: A2A is JSON-RPC 2.0, which flux
/// already speaks (`flux-server`'s A2A route), so the recognizable shape is a JSON object carrying
/// both `jsonrpc` and `method`. Recognizing the protocol flux already implements is not the same as
/// inventing a room-borne wire format — that choice belongs to D-212, and until it is made, the only
/// thing this predicate does is keep the *plain text* bar from also excluding a real envelope.
///
/// Recognizing an envelope is **not** authorizing it. It re-enters the ordinary path — the same
/// `Executor` + approver envelope, and the same reply budget — with no authority it would not have
/// had from a human's line.
pub fn is_a2a_envelope(text: &str) -> bool {
    let text = text.trim();
    if !text.starts_with('{') {
        return false;
    }
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(o)) => {
            o.get("jsonrpc").and_then(|v| v.as_str()) == Some("2.0") && o.contains_key("method")
        }
        _ => false,
    }
}

/// Whether `needle` occurs in `haystack` at word boundaries — both already lowercased.
///
/// Boundary-checked so a nick of `flux` is not matched inside `influx` or `fluxes`. A boundary is
/// any character that is not alphanumeric and not `_`/`-`/`+`, which keeps `@flux` and `flux:` —
/// the two spellings people actually type when addressing a room bot — matching.
fn contains_token(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes_before = |i: usize| haystack[..i].chars().next_back();
    let mut from = 0usize;
    while let Some(offset) = haystack[from..].find(needle) {
        let start = from + offset;
        let end = start + needle.len();
        let before_ok = bytes_before(start).is_none_or(|c| !is_word_char(c));
        let after_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|c| !is_word_char(c));
        if before_ok && after_ok {
            return true;
        }
        // Advance one character, not one byte — `find` returns a char boundary, and so must this.
        from = start + haystack[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// A character that continues a word, for [`contains_token`]'s boundary test.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '+'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public(rule: &AddressRule, text: &str) -> Addressing {
        rule.classify("flux", OccupantKind::Human, MessageScope::Groupchat, text)
    }

    #[test]
    fn the_default_rule_answers_a_mention_and_ignores_the_rest() {
        let rule = AddressRule::default();
        assert_eq!(public(&rule, "flux: when is standup?"), Addressing::Mention);
        assert_eq!(public(&rule, "@flux when is standup?"), Addressing::Mention);
        assert_eq!(public(&rule, "any idea, Flux?"), Addressing::Mention);
        assert_eq!(
            public(&rule, "did the nightly go green?"),
            Addressing::NotAddressed
        );
    }

    #[test]
    fn a_nick_inside_another_word_is_not_a_mention() {
        // The whole failure mode in one line: a room discussing an "influx of tickets" is not
        // addressing the agent, and a substring match would answer every one of those sentences.
        let rule = AddressRule::default();
        assert_eq!(
            public(&rule, "we had an influx of tickets"),
            Addressing::NotAddressed
        );
        assert_eq!(public(&rule, "fluxes everywhere"), Addressing::NotAddressed);
        assert_eq!(public(&rule, "flux-bot is down"), Addressing::NotAddressed);
    }

    #[test]
    fn a_whisper_is_addressed_whatever_the_rule_says() {
        // `never` is the strictest public rule there is, and a private message still gets through:
        // the rule governs public text, because nobody whispers to the agent and does not mean it.
        for rule in [
            AddressRule::never(),
            AddressRule::default(),
            AddressRule::always(),
        ] {
            assert_eq!(
                rule.classify(
                    "flux",
                    OccupantKind::Human,
                    MessageScope::Private,
                    "are you there?"
                ),
                Addressing::Whisper
            );
        }
        assert_eq!(
            public(&AddressRule::never(), "flux: hello?"),
            Addressing::NotAddressed,
            "`never` answers no public line, mention or not"
        );
    }

    #[test]
    fn an_agents_plain_text_is_never_addressed_at_any_scope() {
        let rule = AddressRule::always();
        for scope in [MessageScope::Groupchat, MessageScope::Private] {
            assert_eq!(
                rule.classify(
                    "flux",
                    OccupantKind::Agent,
                    scope,
                    "flux: what do you think?"
                ),
                Addressing::AgentPlainText,
                "an agent's prose is not a turn, {scope:?}"
            );
        }
        // The one shape that does get through — the D-212 seam.
        assert_eq!(
            rule.classify(
                "flux",
                OccupantKind::Agent,
                MessageScope::Groupchat,
                r#"{"jsonrpc":"2.0","method":"message/send","id":1,"params":{}}"#,
            ),
            Addressing::Always
        );
    }

    #[test]
    fn only_a_declared_agent_is_refused_by_kind() {
        // A real MUC reports `Unknown` for everyone (D-205), which is exactly why the reply budget
        // exists as well: this arm alone would never fire on the portable backend.
        let rule = AddressRule::default();
        for kind in [
            OccupantKind::Human,
            OccupantKind::Unknown,
            OccupantKind::Service,
        ] {
            assert_eq!(
                rule.classify("flux", kind, MessageScope::Groupchat, "flux: hi"),
                Addressing::Mention,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_wake_phrase_addresses_without_a_mention() {
        let rule = AddressRule::parse("wake: ok assistant").unwrap();
        assert_eq!(public(&rule, "OK Assistant, take a note"), Addressing::Wake);
        assert_eq!(public(&rule, "flux: take a note"), Addressing::NotAddressed);

        let both = AddressRule::parse("mention, wake: ok assistant").unwrap();
        assert_eq!(public(&both, "flux: take a note"), Addressing::Mention);
        assert_eq!(public(&both, "ok assistant, take a note"), Addressing::Wake);
    }

    #[test]
    fn a_bad_rule_is_a_load_error_rather_than_a_silent_widening() {
        assert_eq!(
            AddressRule::parse("mentoin"),
            Err(AddressRuleError::UnknownToken("mentoin".into()))
        );
        assert_eq!(AddressRule::parse("  "), Err(AddressRuleError::Empty));
        assert_eq!(
            AddressRule::parse("wake:  "),
            Err(AddressRuleError::EmptyWakePhrase)
        );
        assert_eq!(
            AddressRule::parse("always, mention"),
            Err(AddressRuleError::NotCombinable("always"))
        );
        assert_eq!(
            AddressRule::parse("never, mention"),
            Err(AddressRuleError::NotCombinable("never"))
        );
    }

    #[test]
    fn the_documented_vocabulary_round_trips() {
        for spec in ["mention", "always", "never", "mention, wake: ok flux"] {
            let rule = AddressRule::parse(spec).unwrap();
            assert_eq!(
                AddressRule::parse(&rule.to_string()).unwrap(),
                rule,
                "`{spec}` renders as `{rule}`, which must parse back to itself"
            );
        }
        assert_eq!(
            AddressRule::parse(DEFAULT_ADDRESS_RULE).unwrap(),
            AddressRule::default()
        );
    }

    #[test]
    fn an_a2a_envelope_is_recognized_only_in_its_protocol_shape() {
        assert!(is_a2a_envelope(
            r#" {"jsonrpc":"2.0","method":"message/send","params":{}} "#
        ));
        assert!(!is_a2a_envelope(r#"{"method":"message/send"}"#));
        assert!(!is_a2a_envelope(r#"{"jsonrpc":"1.0","method":"x"}"#));
        assert!(!is_a2a_envelope("flux: send me a jsonrpc 2.0 method"));
        assert!(!is_a2a_envelope("[1,2,3]"));
        assert!(!is_a2a_envelope(""));
    }

    #[test]
    fn mention_matching_is_not_confused_by_multibyte_text() {
        // `contains_token` walks by character, so a nick that appears after non-ASCII text must
        // still match — and a byte-wise advance would panic on a char boundary here.
        let rule = AddressRule::default();
        assert_eq!(public(&rule, "café — flux, coffee?"), Addressing::Mention);
        assert_eq!(public(&rule, "flüx, coffee?"), Addressing::NotAddressed);
    }
}
