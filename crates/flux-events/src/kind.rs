//! Event kinds and the append/read envelopes.
//!
//! [`EventKind`] is the **closed set** of facts flux logs. A single Rust enum (not an
//! open type registry) is the right model for a single closed binary: serde gives free
//! (de)serialization and an exhaustive `match` forces every projection to handle every
//! kind at compile time. It is **adjacently tagged** (`{"kind": …, "data": …}`) because
//! two variants wrap another enum/struct ([`EventKind::Run`] wraps [`RunEvent`],
//! [`EventKind::Message`] wraps [`Message`]) — internal tagging cannot flatten a nested
//! enum, and the split mirrors the DB's `kind`-column-plus-`payload`-JSON layout.

use serde::{Deserialize, Serialize};

use flux_core::{Message, Usage};
use flux_lang::ast::RunEvent;

use crate::context::EventContext;

/// The closed set of event kinds in flux's unified log. Adding a kind of fact is one
/// new variant here plus one projection arm — never a new table and new methods.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum EventKind {
    /// A session was created with `model`. The first event in every stream.
    SessionStarted { model: String },
    /// The session's model was switched mid-stream (`/model`).
    ModelChanged { model: String },

    /// One appended provider message. Folding these in stream order rebuilds the
    /// conversation — the projection that replaces `SessionStore::load_messages`.
    Message(Message),
    /// Context compaction: the live conversation was rewritten to `messages`. Append-only,
    /// so the superseded [`EventKind::Message`] events stay in the log; the conversation
    /// projection resets to this snapshot and continues. Replaces the destructive
    /// `SessionStore::rewrite_messages` (which did `DELETE FROM messages`).
    Compacted { messages: Vec<Message> },

    /// A flow run-trace event (replaces the `run_events` table). The inner type is reused
    /// verbatim from `flux-lang` rather than re-defined.
    Run(RunEvent),

    /// A user turn started. The `global_seq` of this event is the `turn_id` that scopes the
    /// turn's [`EventKind::PlanAttempted`] / [`EventKind::TurnEnded`] events.
    TurnStarted { user_input: String, model: String },
    /// One planning attempt within a turn. `outcome` is one of `"accepted"`, `"chat"`,
    /// `"compile_error"`, `"rejected"` (the user declined the plan); `error` carries the diagnostic
    /// when it is `"compile_error"`. For an accepted plan, `fingerprint` is the SHA-256 of the plan
    /// AST's canonical JSON (the loop guard's identity) and `plan_text` is the human-auditable
    /// rendered graph (`render_pretty`, capped) — the durable "a turn is a readable graph" record
    /// (C-14). `phase` is the multi-pass loop phase that produced this attempt — `"orient"`,
    /// `"gather"`, or `"execute"` (A-14) — `None` for pre-A-14 logs and for attempts recorded outside
    /// a phase-aware call. `plan_source` is the accepted plan's CANONICAL parseable Flux-Lang text
    /// (`flux_lang::format::format`, L-38) — machine-minable, round-trips via `parse`; `None` for
    /// non-accepted outcomes, pre-L-38 logs, and oversized plans (never truncated: a present
    /// `plan_source` always parses). All newer fields are `#[serde(default)]` so older logs decode.
    PlanAttempted {
        step: u32,
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fingerprint: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_source: Option<String>,
    },
    /// A turn closed with its final `outcome`, iteration count, and assistant `answer`. `usage` is
    /// the turn's accumulated token tally — `None` for turns recorded before usage capture, or when
    /// the provider reported none. Optional + serde-default so older logs (no `usage`) still decode.
    TurnEnded {
        outcome: String,
        iterations: u32,
        answer: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },

    /// One provider call's token usage, attributed to the `model` that was **actually active** for
    /// that call. A turn can make several calls (the loop calls `plan` once per iteration; a
    /// sub-agent's own calls are folded in too — see `crate::projection::cost_summary`), and
    /// [`EventKind::TurnEnded`]'s `usage` is only the turn's replace-style TOTAL — it can't tell you
    /// which model produced which slice once a mid-turn `/model` switch changes the active planner
    /// (`ModelChanged`). `CallUsage` is the per-call attribution record the `cost_summary` projection
    /// rolls up by `(model, provider)`; `TurnEnded.usage` stays as the turn-total back-compat field
    /// older logs and callers already rely on. Scoped to a turn via `NewEvent::in_turn` like
    /// `PlanAttempted`/`TurnEnded`.
    CallUsage { model: String, usage: Usage },

    /// The host admitted an egress request to a **private/internal** address under a scoped
    /// private-network grant — the auditable security event. `caller` is the plugin name (or
    /// `"web_fetch"`); `host` is the private host that was reached; `grant_source` names the grant
    /// that let it through (e.g. `"config:plugin/<name>"` or `"config:endpoint/<plugin>:<ep>"`).
    PrivateNetAdmit {
        caller: String,
        host: String,
        grant_source: String,
    },

    /// The host materialized a credential owned by one plugin (`provider`) on behalf of a *different*
    /// plugin (`consumer`) under an operator cross-plugin grant — the auditable D-27 security event.
    /// `reference_location` is the `credential_ref` string (a location, e.g.
    /// `kubernetes/<ns>/<name>/<key>`) — **never** the secret value.
    CrossPluginResolve {
        consumer: String,
        provider: String,
        reference_location: String,
    },

    /// The discovery broker fanned a `product` query out to a discovery `provider` and committed
    /// `count` weak endpoint references it returned — the auditable D-30 discovery event. Carries no
    /// URL and no credential (weak refs only): just *which provider discovered how many endpoints for
    /// which product*, so a refresh/reconcile leaves an auditable trail.
    EndpointDiscovered {
        product: String,
        provider: String,
        count: usize,
    },

    /// One evidence observation (`tool_call` markers, `turn.iteration`, `groups.active`,
    /// `skill.activated`, flow-emitted `observe(…)`, …), persisted from the in-memory
    /// `EvidenceLog` by the engine's per-turn watermark flush (C-14). This is what makes the
    /// `/evidence` trail durable — the in-memory log stays the live read model; the event log is
    /// the offline one (`projection::observations`). The inner type is reused verbatim from
    /// `flux-evidence` (L0), never re-defined.
    Observation(flux_evidence::Observation),
}

impl EventKind {
    /// The cheap `&'static str` discriminator written to the indexed `kind` column, so SQL
    /// filters compare a stable, allocation-free tag. Kept identical to serde's tag value.
    pub fn kind_tag(&self) -> &'static str {
        match self {
            EventKind::SessionStarted { .. } => "session_started",
            EventKind::ModelChanged { .. } => "model_changed",
            EventKind::Message(_) => "message",
            EventKind::Compacted { .. } => "compacted",
            EventKind::Run(_) => "run",
            EventKind::TurnStarted { .. } => "turn_started",
            EventKind::PlanAttempted { .. } => "plan_attempted",
            EventKind::TurnEnded { .. } => "turn_ended",
            EventKind::CallUsage { .. } => "call_usage",
            EventKind::PrivateNetAdmit { .. } => "private_net_admit",
            EventKind::CrossPluginResolve { .. } => "cross_plugin_resolve",
            EventKind::EndpointDiscovered { .. } => "endpoint_discovered",
            EventKind::Observation(_) => "observation",
        }
    }
}

/// An event to append. The store mints the `id` (when `None`), assigns `stream_seq` and
/// `global_seq`, and stamps `ts` — so callers only describe *what* happened.
#[derive(Debug, Clone)]
pub struct NewEvent {
    /// A caller-supplied id makes the append idempotent (a retry with the same id is a
    /// no-op returning the prior event). The store mints a ULID when this is `None`.
    pub id: Option<String>,
    /// What happened.
    pub kind: EventKind,
    /// The turn this event belongs to (the `global_seq` of its `TurnStarted`), if any.
    pub turn_id: Option<i64>,
    /// Payload schema version (defaults to 1).
    pub schema_version: u32,
}

impl NewEvent {
    /// A bare event with a store-minted id, no turn scope, schema version 1.
    pub fn new(kind: EventKind) -> Self {
        Self {
            id: None,
            kind,
            turn_id: None,
            schema_version: 1,
        }
    }

    /// Supply a stable id for at-most-once semantics.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Scope this event to a turn (a `TurnStarted`'s `global_seq`).
    pub fn in_turn(mut self, turn_id: i64) -> Self {
        self.turn_id = Some(turn_id);
        self
    }

    /// A conversation message event.
    pub fn message(m: Message) -> Self {
        Self::new(EventKind::Message(m))
    }

    /// A compaction snapshot event.
    pub fn compacted(messages: Vec<Message>) -> Self {
        Self::new(EventKind::Compacted { messages })
    }

    /// A flow run-trace event.
    pub fn run(ev: RunEvent) -> Self {
        Self::new(EventKind::Run(ev))
    }
}

/// An event read back from the log, with its `payload` decoded into [`StoredEvent::kind`].
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    /// Total order across all streams (the table's autoincrement rowid).
    pub global_seq: i64,
    /// The stream (session id, e.g. `"s_42"`) this event belongs to.
    pub stream: String,
    /// 0-based order within the stream.
    pub stream_seq: i64,
    /// The event's stable id (ULID unless caller-supplied).
    pub id: String,
    /// The turn this event belongs to, if any.
    pub turn_id: Option<i64>,
    /// Payload schema version.
    pub schema_version: u32,
    /// Wall-clock timestamp, unix milliseconds.
    pub ts_ms: i64,
    /// The decoded event.
    pub kind: EventKind,
    /// The owning run's tenant/agent context, stamped from the stream registry on read. Empty
    /// ([`EventContext::is_empty`]) for single-tenant sessions and ad-hoc (non-`s_<n>`) streams.
    pub context: EventContext,
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::Usage;

    /// C-34: `Usage.reported_cost_usd` is `#[serde(default, skip_serializing_if = "Option::is_none")]`
    /// — a pre-C-34 `call_usage` row (no such key anywhere in its JSON) must still decode, a fresh
    /// row WITH a reported cost round-trips exactly, and a fresh row with NO reported cost
    /// serializes byte-identically to a pre-C-34 row (the key is entirely absent, not `null`).
    #[test]
    fn call_usage_decodes_pre_c34_rows_and_roundtrips_reported_cost() {
        // A raw pre-C-34 payload, as it would sit in the `events.payload` column today: no
        // `reported_cost_usd` key anywhere in `usage`.
        let pre_c34 = r#"{"kind":"call_usage","data":{"model":"claude-sonnet-4-6","usage":{"input_tokens":100,"output_tokens":20,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"reasoning_tokens":0}}}"#;
        let decoded: EventKind = serde_json::from_str(pre_c34).expect("pre-C-34 row must decode");
        match decoded {
            EventKind::CallUsage { usage, .. } => {
                assert_eq!(
                    usage.reported_cost_usd, None,
                    "an absent key must decode to None, not a spurious Some(0.0) default"
                );
            }
            other => panic!("expected CallUsage, got {other:?}"),
        }

        // A new event WITH a reported cost round-trips exactly.
        let with_cost = EventKind::CallUsage {
            model: "openrouter/deepseek/deepseek-v4-flash:nitro".to_string(),
            usage: Usage {
                input_tokens: 1000,
                output_tokens: 50,
                reported_cost_usd: Some(0.0000020643),
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&with_cost).unwrap();
        assert!(
            json.contains("reported_cost_usd"),
            "a Some reported cost must serialize the key: {json}"
        );
        let round_tripped: EventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, with_cost);

        // A new event with NO reported cost (a non-reporting provider) serializes WITHOUT the key
        // at all — byte-stable with every pre-C-34 row already on disk.
        let without_cost = EventKind::CallUsage {
            model: "anthropic/claude-sonnet-4-6".to_string(),
            usage: Usage {
                input_tokens: 1000,
                output_tokens: 50,
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&without_cost).unwrap();
        assert!(
            !json.contains("reported_cost_usd"),
            "None must not serialize the key at all: {json}"
        );
    }
}
