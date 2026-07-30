//! Evidence-pinned cross-session memory: the entry schema and its stream naming (A-107).
//!
//! Memory is cross-session, so it cannot live on a session stream. It gets its **own** stream —
//! `memory:<scope-key>` — in the *same* `events.db`, following the store's canon: append-only, one
//! log, projections as the read model ([`crate::memory_entries`]). Nothing here is a side
//! table, which is what buys multi-process safety (C-25/C-125), WAL hygiene (C-126), the Postgres
//! backend and the existing read tooling for free rather than re-earning each of them.
//!
//! **The load-bearing invariant** (design: `docs/designs/evidence-pinned-memory.md`):
//!
//! > The model supplies the claim. The host supplies the citation.
//!
//! Every field below except [`MemoryNote`]'s claim is host-stamped. This module deliberately
//! carries no constructor through which a caller could hand over an entry whose citation it
//! invented — [`MemoryNote::new`] takes the raw claim plus an already-resolved [`Receipt`], and the
//! store stamps the id and the timestamp on top.
//!
//! ### Why [`EventKind::Custom`] rather than new enum variants
//!
//! [`EventKind`](crate::EventKind) is deliberately *closed* and deliberately **not**
//! `#[non_exhaustive]`, so three new variants (`MemoryNoted`/`MemoryEdited`/`MemoryForgotten`)
//! would be a breaking change for every downstream `match` — for a fact flux's own closed
//! projections (conversation, cost, turns, evidence) never need to understand. `Custom` is the
//! sanctioned open extension point for exactly that, and it keeps A-107 additive. The cost is that
//! the payload shape is not compile-checked; [`crate::memory_entries`] therefore *skips* a
//! `memory.*` event whose payload does not decode rather than failing the read, mirroring
//! `decode_all`'s skip-and-continue discipline for a row it cannot understand.

use serde::{Deserialize, Serialize};

/// Where a memory entry applies, and therefore which stream it lives on.
///
/// The scope key is the stream suffix, so it must be stable across releases — it is a storage
/// identifier, not a display string. `Global` is `"global"`; a project scope is `"project:<key>"`,
/// namespaced so a project whose key happened to be `"global"` can never collide with the global
/// stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum MemoryScope {
    /// Applies everywhere this agent runs.
    Global,
    /// Applies only within one project/workspace, identified by `key`.
    Project { key: String },
}

impl MemoryScope {
    /// The prefix every memory stream carries. Public so a retention or export job can recognise a
    /// memory stream without re-deriving the naming rule.
    pub const STREAM_PREFIX: &'static str = "memory:";

    /// The stable scope key — the `<scope-key>` in `memory:<scope-key>`.
    pub fn scope_key(&self) -> String {
        match self {
            MemoryScope::Global => "global".to_string(),
            MemoryScope::Project { key } => format!("project:{key}"),
        }
    }

    /// The stream this scope's entries are appended to.
    pub fn stream(&self) -> String {
        format!("{}{}", Self::STREAM_PREFIX, self.scope_key())
    }
}

/// The event-store citation a memory entry was learned from — host-stamped, never model-supplied.
///
/// `event_id` is the cited event's **stable id** (a ULID), *not* its `global_seq`. That choice is
/// load-bearing rather than stylistic: `global_seq` is a backend rowid. It is re-minted by any
/// re-import or store migration, and it does not mean the same thing across the SQLite and
/// Postgres backends, so a citation pinned to it silently comes to name a *different* event — the
/// worst possible failure for a provenance record, because it still resolves. The stable id is
/// caller-stable, is already the basis of C-125's cross-process idempotency proof, and survives
/// both. [`EventStore::resolve_receipt`](crate::EventStore::resolve_receipt) is the only supported
/// way to follow one back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// The stream the cited event lives on (usually the learning session, `s_<n>`).
    pub stream: String,
    /// The cited event's stable id (ULID) — **never** a `global_seq`.
    pub event_id: String,
    /// The turn the claim was learned in, when it was learned inside one.
    ///
    /// This is the store's existing turn handle — the `global_seq` of that turn's `TurnStarted`
    /// (see [`EventKind::TurnStarted`](crate::EventKind::TurnStarted)) — and is therefore
    /// **store-local**, unlike `event_id`. It is a convenience for narrowing a `load_turn` read in
    /// the store that wrote it, not part of the durable citation: `event_id` alone is what has to
    /// survive a migration, and it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<i64>,
}

/// The workspace revision a claim was learned at, plus the paths the learning turn actually
/// touched — what makes staleness computable later (A-109). `None` when the claim was learned
/// outside a git repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPin {
    /// `git rev-parse HEAD` at the moment of learning.
    pub sha: String,
    /// The workspace paths the citing turn read or wrote — taken from the turn's evidence trail,
    /// never from the model.
    pub paths: Vec<String>,
}

/// One durable, evidence-pinned memory entry — the projection's element and the payload of a
/// `memory.noted` / `memory.edited` event.
///
/// `claim` is the only model-authored field. Everything else is stamped by the host, which is what
/// keeps the citation from being forgeable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// Stable across edits: a ULID, and also the id of the `memory.noted` event that first
    /// recorded this entry — no second id scheme (mirrors A-98's wake-up identity).
    pub id: String,
    /// The model's contribution, already scrubbed — see [`MemoryNote::new`].
    pub claim: String,
    /// Which scope (and therefore which stream) this entry belongs to.
    pub scope: MemoryScope,
    /// The event-store citation this claim was learned from.
    pub receipt: Receipt,
    /// The workspace revision it was learned at, when learned inside a repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitPin>,
    /// When the entry reached its current state, unix milliseconds. An edit re-stamps it: the
    /// claim was re-learned, and the superseded state stays readable in the log.
    pub learned_at_ms: i64,
}

/// The write shape: a claim plus the citation the host resolved for it.
///
/// **The claim field is private and there is exactly one constructor**, [`MemoryNote::new`], which
/// takes the scrub function. That is deliberate: it makes "redact before the store sees it" a
/// property of the type rather than a convention a future call site can forget — the same
/// by-construction discipline [`SessionLog`](crate::SessionLog) applies to the session-shape
/// invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryNote {
    /// Post-scrub. Private so it can only be set through [`MemoryNote::new`].
    claim: String,
    /// The host-resolved citation.
    pub receipt: Receipt,
    /// The workspace pin, when there is one.
    pub git: Option<GitPin>,
}

impl MemoryNote {
    /// Build a note from the model's `raw_claim`, scrubbing it through `redact` on the way in.
    ///
    /// `redact` is the **live turn's [`flux_secret::Redactor::redact`]** — the same scrub
    /// `flux-flow`'s `flush_observations` applies at the evidence flush seam (C-22/C-164), seeded
    /// from `resolve_secrets` with every credential the run materialized. It is passed in rather
    /// than reached for because redaction is a *caller* responsibility everywhere in this crate:
    /// `flux-events` owns no scrubber of its own and must not grow a second one, so the store only
    /// ever sees already-scrubbed text. Passing the real redactor also carries the registered-value
    /// set, which a copy living here could not see.
    ///
    /// [`flux_secret::Redactor::redact`]: https://docs.rs/codewandler-flux-secret
    pub fn new(
        raw_claim: &str,
        receipt: Receipt,
        git: Option<GitPin>,
        redact: impl Fn(&str) -> String,
    ) -> Self {
        Self {
            claim: redact(raw_claim),
            receipt,
            git,
        }
    }

    /// The scrubbed claim.
    pub fn claim(&self) -> &str {
        &self.claim
    }
}

/// The payload of a `memory.forgotten` event: a tombstone naming the entry that is no longer
/// believed. Nothing is deleted — the projection stops surfacing the id, the log keeps every state
/// it ever held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryTombstone {
    /// The [`MemoryEntry::id`] this tombstone retires.
    pub id: String,
}

/// `EventKind::Custom` name for "a new entry was learned". Payload: a [`MemoryEntry`].
pub const MEMORY_NOTED: &str = "memory.noted";
/// `EventKind::Custom` name for "an existing entry was re-stated". Payload: the **whole** new
/// [`MemoryEntry`] for that id — an edit appends a complete replacement state rather than a patch,
/// so folding is `latest wins` with no merge rules to get wrong.
pub const MEMORY_EDITED: &str = "memory.edited";
/// `EventKind::Custom` name for "this entry is no longer believed". Payload: a
/// [`MemoryTombstone`].
pub const MEMORY_FORGOTTEN: &str = "memory.forgotten";

/// The reserved `Custom` name prefix A-107 folds. An embedder writing its own app facts must stay
/// out of it (see [`EventKind::Custom`](crate::EventKind::Custom)'s doc).
pub const MEMORY_NAME_PREFIX: &str = "memory.";

#[cfg(test)]
mod tests {
    use super::*;

    /// The scope key is a storage identifier: the exact strings below are baked into stream names
    /// already on disk, so a change here silently orphans every entry written before it.
    #[test]
    fn scope_keys_and_stream_names_are_stable() {
        assert_eq!(MemoryScope::Global.scope_key(), "global");
        assert_eq!(MemoryScope::Global.stream(), "memory:global");
        let p = MemoryScope::Project {
            key: "flux".to_string(),
        };
        assert_eq!(p.scope_key(), "project:flux");
        assert_eq!(p.stream(), "memory:project:flux");
        // A project literally keyed "global" must not collide with the global stream.
        let shadow = MemoryScope::Project {
            key: "global".to_string(),
        };
        assert_ne!(shadow.stream(), MemoryScope::Global.stream());
    }

    /// The entry payload round-trips byte-stably, and the optional `git` pin is *absent* (not
    /// `null`) when there is none — so an entry learned outside a repo serializes identically to
    /// one written by a build that predates the field.
    #[test]
    fn entry_round_trips_and_omits_an_absent_git_pin() {
        let entry = MemoryEntry {
            id: "01J000000000000000000000AA".to_string(),
            claim: "the auth middleware lives in src/mw/auth.rs".to_string(),
            scope: MemoryScope::Project {
                key: "flux".to_string(),
            },
            receipt: Receipt {
                stream: "s_7".to_string(),
                event_id: "01J000000000000000000000BB".to_string(),
                turn_id: Some(42),
            },
            git: None,
            learned_at_ms: 1_700_000_000_000,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(
            !json.contains("\"git\""),
            "an absent pin must not serialize: {json}"
        );
        assert_eq!(
            serde_json::from_str::<MemoryEntry>(&json).unwrap(),
            entry,
            "the payload must round-trip exactly"
        );

        let pinned = MemoryEntry {
            git: Some(GitPin {
                sha: "a1b2c3d".to_string(),
                paths: vec!["src/mw/auth.rs".to_string()],
            }),
            ..entry
        };
        let json = serde_json::to_string(&pinned).unwrap();
        assert_eq!(serde_json::from_str::<MemoryEntry>(&json).unwrap(), pinned);
    }

    /// The scope tag is internally tagged, so the on-disk shape is self-describing and a global
    /// scope carries no stray `key`.
    #[test]
    fn scope_serializes_with_a_self_describing_tag() {
        assert_eq!(
            serde_json::to_value(MemoryScope::Global).unwrap(),
            serde_json::json!({"scope": "global"})
        );
        assert_eq!(
            serde_json::to_value(MemoryScope::Project {
                key: "flux".to_string()
            })
            .unwrap(),
            serde_json::json!({"scope": "project", "key": "flux"})
        );
    }

    /// `MemoryNote` has exactly one constructor and it applies the scrub — there is no field
    /// assignment path that reaches the store with raw model text.
    #[test]
    fn note_construction_applies_the_scrub() {
        let note = MemoryNote::new(
            "token sk-ant-api03-AAAABBBBCCCCDDDD",
            Receipt {
                stream: "s_1".to_string(),
                event_id: "01J000000000000000000000CC".to_string(),
                turn_id: None,
            },
            None,
            |s| s.replace("sk-ant-api03-AAAABBBBCCCCDDDD", "[redacted]"),
        );
        assert_eq!(note.claim(), "token [redacted]");
    }
}
