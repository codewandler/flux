//! [`Speaker`] — who spoke a turn.
//!
//! The turn seam grew up on a phone line, where there is exactly one candidate, so
//! [`VoiceTurnHandler::turn`](super::VoiceTurnHandler::turn) carried only text. A meeting room
//! (D-204) has N speakers and the agent is the addressee of almost none of them, so attribution is
//! not a feature of the room — it is the precondition for deciding whether to answer at all.
//!
//! The identity is deliberately a plain string the *surface* owns: an XMPP occupant JID, a SIP
//! caller number, a room backend's opaque handle. L3 never interprets it, it only carries it, which
//! is what keeps this type below the L6 crates that mint the ids.

/// The speaker id a 1:1 surface uses — a phone line has one candidate, so every turn on it is
/// attributed to the same synthetic speaker rather than to nobody.
pub const SOLE_SPEAKER_ID: &str = "caller";

/// Who spoke one turn: a surface-owned stable id, plus the room-visible name when the surface knows
/// one. Compare on [`id`](Self::id) — a display name is cosmetic and two occupants may share it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Speaker {
    id: String,
    display_name: Option<String>,
}

impl Speaker {
    /// A speaker known only by id.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: None,
        }
    }

    /// The single caller of a 1:1 surface — the pre-room behavior, named rather than absent.
    pub fn sole() -> Self {
        Self::new(SOLE_SPEAKER_ID)
    }

    /// Attach the room-visible name (an XMPP nick, a Slack display name).
    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    /// The stable, surface-owned identity. This is the field to compare and to key context on.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The room-visible name, when the surface knows one.
    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    /// The best human-readable label: the display name, else the id. Safe to put in prose.
    pub fn label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sole_speaker_is_named_not_absent() {
        let sole = Speaker::sole();
        assert_eq!(sole.id(), SOLE_SPEAKER_ID);
        assert_eq!(
            sole.label(),
            SOLE_SPEAKER_ID,
            "the id is the fallback label"
        );
        assert_eq!(sole.display_name(), None);
    }

    #[test]
    fn identity_is_the_id_not_the_display_name() {
        let a = Speaker::new("room@x/ada").with_display_name("ada");
        let b = Speaker::new("room@x/ada2").with_display_name("ada");
        assert_ne!(a, b, "a shared nick is not a shared identity");
        assert_eq!(a.label(), "ada");
    }
}
