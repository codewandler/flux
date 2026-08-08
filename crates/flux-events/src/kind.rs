//! Event kinds and the append/read envelopes.
//!
//! The set of **flux** facts — the things the engine itself decides happened (a turn started,
//! a plan was attempted, a call billed tokens, …) — is **closed** and stays that way. A single
//! Rust enum (not an open type registry) is the right model for those: serde gives free
//! (de)serialization and an exhaustive `match` forces every projection to handle every flux
//! kind at compile time, so a new fact can never silently fail to show up in the conversation,
//! cost, or evidence read models.
//!
//! [`EventKind::Custom`] is the **one** deliberate extension point, for **app** facts: a
//! consumer embedding flux (an audit trail, a domain event, …) gets to ride the same
//! append-only, account-scoped, ordered log instead of standing up a parallel store, without
//! flux ever needing to understand what the fact means. It stays a variant of this same enum
//! (not a bypass around it) so the exhaustive-match discipline still applies — adding it forced
//! one arm onto every existing match, and every future flux match must keep deciding what
//! `Custom` means for it (almost always: pass it through unread). The enum is deliberately kept
//! *not* `#[non_exhaustive]` for exactly that reason: an open registry inside a closed enum
//! would defeat the compile-time guarantee the module exists to give.
//!
//! It is **adjacently tagged** (`{"kind": …, "data": …}`) because several variants wrap another
//! enum/struct ([`EventKind::Run`] wraps [`RunEvent`], [`EventKind::Message`] wraps [`Message`])
//! — internal tagging cannot flatten a nested enum, and the split mirrors the DB's
//! `kind`-column-plus-`payload`-JSON layout.

use serde::{Deserialize, Serialize};

use flux_core::{AgentLoopBindingMetadata, Message, Usage};
use flux_lang::ast::RunEvent;

use crate::context::EventContext;

/// The event kinds in flux's unified log: a closed set of *flux* facts, plus the single open
/// [`EventKind::Custom`] extension point for *app* facts. Adding a kind of **flux** fact is one
/// new variant here plus one projection arm — never a new table and new methods; an **app**
/// fact needs no new variant at all — it rides [`EventKind::Custom`].
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
    TurnStarted {
        user_input: String,
        model: String,
        /// Resolved behavior harness for this turn. Absent only on pre-binding/legacy writes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        loop_binding: Option<AgentLoopBindingMetadata>,
    },
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
    /// `plan_source` always parses). `delta_source` is the raw legacy `emit_plan_delta` tool input
    /// (canonical JSON) when this accepted attempt was produced by patching the previous rejected
    /// plan rather than re-emitting it whole (KF3/L-55) — `None` for every ordinary full emission,
    /// so the durable record can reconstruct BOTH the patch that was sent and (via `plan_source`)
    /// the full plan it materialized into. All newer fields are `#[serde(default)]` so older logs
    /// decode.
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delta_source: Option<String>,
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
        /// Repeated at the terminal boundary so a bounded receipt is self-contained.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        loop_binding: Option<AgentLoopBindingMetadata>,
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
    /// `"web.fetch"`); `host` is the private host that was reached; `grant_source` names the grant
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

    /// A turn registered a future wake-up (A-98): resume THIS session later with `prompt` (plus
    /// optional `context` captured now, replayed back unchanged) once `fire_at_ms` elapses. The
    /// wake-up's identity is this event's own store-minted `id` — no separate id scheme.
    WakeupScheduled {
        fire_at_ms: i64,
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<String>,
    },
    /// A previously scheduled wake-up fired: its session was re-entered (via the ordinary
    /// `FlowEngine::run_turn` path, not a bespoke route) as `turn_id` (A-98, the C-26 lesson — a
    /// real turn, not a telemetry-free shortcut). `wakeup_id` is the firing `WakeupScheduled`
    /// event's `id`.
    WakeupFired { wakeup_id: String, turn_id: i64 },
    /// A pending wake-up was cancelled before it fired (A-98) — an explicit `flux wakeups cancel`,
    /// or (implicitly, via whole-stream deletion) the session it belonged to being pruned/deleted.
    WakeupCancelled { wakeup_id: String },

    /// An **app-defined** fact (D-55) — the one open extension point in an otherwise closed enum.
    /// `name` namespaces the fact so unrelated consumers sharing one log don't collide; dotted
    /// names are recommended (e.g. `"audit.tool_call"`, mirroring the `Observation.kind` string
    /// convention). `payload` is opaque — **flux never interprets it**, never validates its shape,
    /// and never folds it into any flux projection; it exists purely so the consumer that wrote it
    /// can read it back, byte-for-byte, through the ordered, account-scoped log instead of a
    /// parallel store. Every flux projection must still decide what `Custom` means for *it*
    /// (compile-forced by the exhaustive match) — the answer is almost always "skip".
    ///
    /// **One reserved namespace** (A-107): `name`s beginning with `"memory."` are flux's own —
    /// cross-session memory rides `Custom` (rather than three new closed variants, which would
    /// break every downstream `match`) and *is* folded, by
    /// [`memory_entries`](crate::memory_entries), off the `memory:<scope-key>` streams. An embedder
    /// must namespace its app facts away from that prefix. A colliding, undecodable payload is
    /// skipped by the fold rather than erroring the read, so a collision degrades one entry instead
    /// of the whole memory read model — but it is still a collision, and the prefix is reserved.
    Custom {
        name: String,
        payload: serde_json::Value,
    },
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
            EventKind::WakeupScheduled { .. } => "wakeup_scheduled",
            EventKind::WakeupFired { .. } => "wakeup_fired",
            EventKind::WakeupCancelled { .. } => "wakeup_cancelled",
            EventKind::Custom { .. } => "custom",
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

    /// C-38: `Usage.audio_input_tokens`/`audio_output_tokens` are plain `#[serde(default)]` `u64`s
    /// (unlike `reported_cost_usd`, always serialized — no `skip_serializing_if`) — a pre-C-38
    /// `call_usage` row (neither key present anywhere in its JSON) must still decode with both
    /// audio fields zeroed, and a fresh row with non-zero audio counts round-trips exactly.
    #[test]
    fn call_usage_decodes_pre_c38_rows_and_roundtrips_audio_tokens() {
        // A raw pre-C-38 payload: no `audio_input_tokens`/`audio_output_tokens` key anywhere.
        let pre_c38 = r#"{"kind":"call_usage","data":{"model":"claude-sonnet-4-6","usage":{"input_tokens":100,"output_tokens":20,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"reasoning_tokens":0}}}"#;
        let decoded: EventKind = serde_json::from_str(pre_c38).expect("pre-C-38 row must decode");
        match decoded {
            EventKind::CallUsage { usage, .. } => {
                assert_eq!(usage.audio_input_tokens, 0, "absent key must default to 0");
                assert_eq!(usage.audio_output_tokens, 0, "absent key must default to 0");
            }
            other => panic!("expected CallUsage, got {other:?}"),
        }

        // A new event WITH audio usage round-trips exactly.
        let with_audio = EventKind::CallUsage {
            model: "openai/gpt-realtime".to_string(),
            usage: Usage {
                input_tokens: 1000,
                output_tokens: 200,
                audio_input_tokens: 400,
                audio_output_tokens: 150,
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&with_audio).unwrap();
        assert!(json.contains("audio_input_tokens"));
        assert!(json.contains("audio_output_tokens"));
        let round_tripped: EventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, with_audio);
    }

    /// D-55: `Custom` is adjacently tagged exactly like every other variant
    /// (`{"kind":"custom","data":{...}}`, not internally tagged, not flattened) and round-trips its
    /// opaque `payload` byte-for-byte regardless of shape (object, array, scalar).
    #[test]
    fn custom_round_trips_with_adjacent_tag_shape() {
        let kind = EventKind::Custom {
            name: "audit.tool_call".to_string(),
            payload: serde_json::json!({"tool": "read_file", "path": "a.rs", "bytes": 128}),
        };

        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "custom",
                "data": {
                    "name": "audit.tool_call",
                    "payload": {"tool": "read_file", "path": "a.rs", "bytes": 128}
                }
            }),
            "Custom must be adjacently tagged like every other EventKind variant: {json}"
        );

        let round_tripped: EventKind = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped, kind);
        assert_eq!(round_tripped.kind_tag(), "custom");

        // The payload is opaque: flux never validates its shape. A non-object payload round-trips
        // just as well.
        let scalar_payload = EventKind::Custom {
            name: "app.counter".to_string(),
            payload: serde_json::json!(42),
        };
        let json = serde_json::to_string(&scalar_payload).unwrap();
        let decoded: EventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, scalar_payload);
    }

    /// D-55: adding `EventKind::Custom` must not perturb decoding of any pre-existing kind already
    /// on disk — every case here is a raw JSON payload shaped exactly as a pre-D-55 log row, with no
    /// `custom` key anywhere near it.
    #[test]
    fn pre_d55_logs_without_custom_still_decode() {
        let cases: &[(&str, &str)] = &[
            (
                r#"{"kind":"session_started","data":{"model":"m"}}"#,
                "session_started",
            ),
            (
                r#"{"kind":"turn_started","data":{"user_input":"hi","model":"m"}}"#,
                "turn_started",
            ),
            (
                r#"{"kind":"call_usage","data":{"model":"m","usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"reasoning_tokens":0}}}"#,
                "call_usage",
            ),
            (
                r#"{"kind":"turn_ended","data":{"outcome":"accepted","iterations":1,"answer":"done"}}"#,
                "turn_ended",
            ),
        ];
        for (raw, tag) in cases {
            let decoded: EventKind = serde_json::from_str(raw)
                .unwrap_or_else(|e| panic!("{tag} must still decode: {e}"));
            assert_eq!(decoded.kind_tag(), *tag, "decoded the wrong kind for {raw}");
        }
    }
}
