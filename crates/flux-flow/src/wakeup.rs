//! `schedule_wakeup` — the agent-set wake-up op (A-98): let a live turn register a durable future
//! continuation of *its own* session, plus the shared servicing step that fires a due wake-up
//! through the existing turn path. Full contract: `docs/designs/agent-set-wakeup.md`.
//!
//! Registration rides the event log, never a new store (`EventStore::schedule_wakeup` /
//! `pending_wakeups` / `due_wakeups` / `cancel_wakeup` / `mark_wakeup_fired`) — this module only
//! adds the agent-facing op and the turn-path servicing driver over that shipped machinery.
//!
//! Policy: the op declares `Effect::LocalSystem` + `AccessKind::LocalSystem` under its own name, so
//! the DEFAULT `Tool::authority_requirements` adapter derives
//! `AuthorityRequirement::host_write("schedule_wakeup")` — the same derivation a `settings.save`-style
//! op gets, with no override. That resolves against the **existing** `host.write` default grant
//! (already `requires_approval: true` for every subject), so registering a wake-up prompts for
//! approval exactly like any other host-state mutation. Deliberately NOT a new domain-specific
//! semantic-effect tag (see the design doc — the C-184 lesson: consequence classes, not domain
//! nouns). The per-session cap and maximum horizon are enforced here, against the durable
//! projection, NOT via a `LoopHost` per-turn reservation: `LoopHost` accounting explicitly resets
//! every turn, which does not fit a bound that must survive across many turns of the same session.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use flux_core::{render_knowledge_blocks, ContextBlock, Error, Result};
use flux_events::{EventStore, PendingWakeup};
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{tool_input_schema, AccessKind, Effect, Idempotency, Risk, ToolSpec};

use crate::agent_sink::AgentSink;
use crate::engine::FlowEngine;

/// Maximum horizon (seconds) a wake-up may be scheduled for when `[wakeup] max_horizon_secs` is
/// not configured: 24 hours.
pub const DEFAULT_MAX_HORIZON_SECS: u64 = 24 * 3600;

/// Maximum number of wake-ups that may be pending at once on one session when `[wakeup]
/// max_pending_per_session` is not configured.
pub const DEFAULT_MAX_PENDING_PER_SESSION: usize = 5;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `schedule_wakeup` op input.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WakeupInput {
    /// The prompt this session will be woken with — becomes the "user" input of the turn this
    /// wake-up fires as.
    prompt: String,
    /// Extra background this wake-up needs, captured now and replayed back unchanged (contained,
    /// never treated as fresh instructions) once it fires.
    #[serde(default)]
    context: Option<String>,
    /// How many seconds from now to wake — bounded by the configured maximum horizon.
    in_secs: u64,
}

/// The `schedule_wakeup` op (A-98): registers a durable future continuation of the calling
/// session. See the module docs and `docs/designs/agent-set-wakeup.md`.
pub struct WakeupTool {
    events: Arc<EventStore>,
    max_horizon_secs: u64,
    max_pending_per_session: usize,
}

impl WakeupTool {
    /// Build the op. `max_horizon_secs`/`max_pending_per_session` are the resolved `[wakeup]`
    /// config values (defaults applied by the caller — see [`DEFAULT_MAX_HORIZON_SECS`]/
    /// [`DEFAULT_MAX_PENDING_PER_SESSION`]).
    pub fn new(
        events: Arc<EventStore>,
        max_horizon_secs: u64,
        max_pending_per_session: usize,
    ) -> Self {
        Self {
            events,
            max_horizon_secs,
            max_pending_per_session,
        }
    }

    /// Register `schedule_wakeup` into `registry`.
    pub fn try_register(self, registry: &mut ToolRegistry) -> Result<()> {
        registry.try_register_from("flux-flow schedule_wakeup operation", Arc::new(self))
    }
}

#[async_trait]
impl Tool for WakeupTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "schedule_wakeup".into(),
            description: "Register a future wake-up on THIS session: after `in_secs` seconds, the \
                          session is resumed with `prompt` as if it were a new message, carrying \
                          along any `context` captured now. Use this to end a turn while something \
                          else is still in flight (\"the deploy is running; check back in 10 \
                          minutes\") instead of blocking or losing the thread. Requires approval; \
                          bounded by a configured maximum horizon and a per-session cap on how many \
                          wake-ups may be pending at once. Returns the wake-up's id."
                .into(),
            input_schema: tool_input_schema::<WakeupInput>(),
            output_schema: None,
            effects: vec![Effect::LocalSystem],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::LocalSystem],
            group: None,
        }
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: WakeupInput = serde_json::from_value(params)
            .map_err(|e| Error::Other(format!("schedule_wakeup: invalid arguments: {e}")))?;
        if args.prompt.trim().is_empty() {
            return Err(Error::Other(
                "schedule_wakeup: `prompt` must not be empty".into(),
            ));
        }
        if args.in_secs == 0 {
            return Ok(ToolResult::error(
                "schedule_wakeup: `in_secs` must be greater than zero",
            ));
        }
        if args.in_secs > self.max_horizon_secs {
            return Ok(ToolResult::error(format!(
                "schedule_wakeup: `in_secs` ({}) exceeds the configured maximum horizon of {}s",
                args.in_secs, self.max_horizon_secs
            )));
        }
        let Some(session) = ctx.session_id() else {
            return Err(Error::Other(
                "schedule_wakeup: no active session to schedule a wake-up on".into(),
            ));
        };
        // Per-session cap (A-98): read the durable pending count directly rather than a LoopHost
        // reservation — see the module doc for why.
        let pending = self.events.pending_wakeups(&session)?;
        if pending.len() >= self.max_pending_per_session {
            return Ok(ToolResult::error(format!(
                "schedule_wakeup: session already has {} pending wake-up(s), at the configured \
                 cap of {}",
                pending.len(),
                self.max_pending_per_session
            )));
        }
        let fire_at_ms = now_ms() + (args.in_secs as i64) * 1000;
        let wakeup_id = self.events.schedule_wakeup(
            &session,
            fire_at_ms,
            &args.prompt,
            args.context.as_deref(),
        )?;
        Ok(ToolResult::ok(format!(
            "Wake-up `{wakeup_id}` scheduled in {}s. Cancel with `flux wakeups cancel {session} {wakeup_id}`.",
            args.in_secs
        )))
    }
}

/// One line [`service_due_wakeups_on_open`] wants surfaced (mirrors `resurrect::OnOpenLine`) — the
/// caller decides how to render it, but every line must be shown somewhere loud, never buffered
/// and silently dropped.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum WakeupLine {
    /// A wake-up is about to fire, or a batch finished.
    Info(String),
    /// The step could not proceed for one wake-up (the turn errored, or recording it fired
    /// failed) — reported, never silent, and never aborts the rest of the due batch.
    Error(String),
}

/// A-98: fire every wake-up on `session_id` whose `fire_at_ms` has already elapsed, oldest-due
/// first — the "fires on next open" answer to "who services it" (see the design doc's "Who
/// services it" section). Every wake-up fires through the **existing** turn path —
/// `agent.run_turn(session_id, composed_input, sink)`, the exact call an ordinary follow-up
/// message or a suspension resume already uses — so telemetry/turn id/correlation are correct for
/// free (the C-26 lesson: no bespoke second execution route) and the fired turn's spend lands
/// under `session_id` like any other turn (cost attribution). Returns the number fired.
///
/// Intended to run at the same "before the caller's own new turn" call site
/// `resurrect_on_open`/D-183 already established (today wired into the plain `flux` CLI's
/// one-shot run + REPL; not yet the TUI or the SDK — see the design doc's explicit scope note).
pub async fn service_due_wakeups_on_open(
    agent: &FlowEngine,
    session_id: &str,
    sink: &mut dyn AgentSink,
    mut report: impl FnMut(WakeupLine),
) -> usize {
    let due = match agent.events.due_wakeups(session_id, now_ms()) {
        Ok(due) => due,
        Err(e) => {
            report(WakeupLine::Error(format!(
                "wakeup · session {session_id}: {e}"
            )));
            return 0;
        }
    };
    let mut fired = 0;
    for w in due {
        report(WakeupLine::Info(format!(
            "wakeup · session {session_id} · firing `{}` — nothing was watching live, so it \
             fires now on session open",
            w.wakeup_id
        )));
        let input = compose_wakeup_input(&w);
        if let Err(e) = agent.run_turn(session_id, &input, sink).await {
            report(WakeupLine::Error(format!(
                "wakeup `{}` failed to fire: {e}",
                w.wakeup_id
            )));
        }
        // Whether the turn errored or not, `run_turn` closes it durably (every termination path
        // does) — the just-closed turn is this session's newest. Consume the wake-up regardless
        // (at-most-once firing): a systematically-failing wake-up must not retry forever.
        let turn_id = agent
            .events
            .turns(session_id)
            .ok()
            .and_then(|ts| ts.last().map(|t| t.turn_id))
            .unwrap_or(-1);
        if let Err(e) = agent
            .events
            .mark_wakeup_fired(session_id, &w.wakeup_id, turn_id)
        {
            report(WakeupLine::Error(format!(
                "wakeup: failed to record `{}` as fired: {e}",
                w.wakeup_id
            )));
        }
        fired += 1;
    }
    fired
}

/// Compose the fired turn's input: the prompt alone, or the prompt with its captured `context`
/// wrapped in a [`ContextBlock`] (the same containment machinery `consult`/A-21 knowledge
/// injection use) so it is never treated as fresh instructions however old — or hostile — its
/// content turns out to be by the time it resurfaces.
fn compose_wakeup_input(w: &PendingWakeup) -> String {
    match w.context.as_deref().map(str::trim) {
        Some(context) if !context.is_empty() => {
            let block = ContextBlock::new(
                "wakeup-context",
                "background captured when this wake-up was scheduled",
                context.to_string(),
            );
            format!(
                "{}\n\n{}",
                render_knowledge_blocks(std::slice::from_ref(&block), 0),
                w.prompt
            )
        }
        _ => w.prompt.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    use flux_core::{Chunk, ContentBlock, StopReason};
    use flux_provider::{ChunkStream, Provider, Request};
    use flux_runtime::{
        AllowApprover, AuthorityRequirement, PermissionManager, ToolContext as RuntimeToolContext,
    };
    use flux_system::{System, Workspace};
    use serde_json::json;

    use crate::engine::AgentLoopSpec;
    use crate::state::FlowStore;

    static TEST_ROOT: AtomicU64 = AtomicU64::new(0);

    /// A no-op sink — every [`AgentSink`] method defaults, so this only exists to have a concrete
    /// `&mut dyn AgentSink` to pass (mirrors `resurrect::tests::NullSink`).
    #[derive(Default)]
    struct NullSink;
    impl AgentSink for NullSink {}

    fn ctx() -> ToolContext {
        let dir = std::env::temp_dir().join(format!(
            "flux-flow-wakeup-test-{}-{}",
            std::process::id(),
            TEST_ROOT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        RuntimeToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())))
    }

    fn tool(events: Arc<EventStore>) -> WakeupTool {
        WakeupTool::new(
            events,
            DEFAULT_MAX_HORIZON_SECS,
            DEFAULT_MAX_PENDING_PER_SESSION,
        )
    }

    /// A-98: the op declares exactly `Effect::LocalSystem` + `AccessKind::LocalSystem` under its
    /// own name — the DEFAULT authority adapter (no override in this op) must therefore derive
    /// `host_write("schedule_wakeup")`, which resolves against the EXISTING `host.write` default
    /// grant (approval-gated for every subject) — "requires authority" with zero new policy code.
    #[test]
    fn schedule_wakeup_declares_a_host_write_authority_requirement() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let t = tool(events);
        let spec = t.spec();
        assert_eq!(spec.name, "schedule_wakeup");
        assert_eq!(spec.effects, vec![Effect::LocalSystem]);
        assert_eq!(spec.access, vec![AccessKind::LocalSystem]);
        let reqs = t.authority_requirements(&json!({}), &[]).unwrap();
        assert_eq!(
            reqs,
            vec![AuthorityRequirement::host_write("schedule_wakeup")],
            "must resolve through the default host_write derivation, not a bespoke action"
        );
    }

    /// A-98 failing-first (durability + context intact): registering a wake-up through the op
    /// persists it as an event via `EventStore`, and it rehydrates with prompt + context intact
    /// through the ordinary `pending_wakeups` read — the same store a process restart would reopen
    /// (the store-level reopen proof lives in `flux-events`; this proves the op writes through the
    /// identical path, not a parallel one).
    #[tokio::test]
    async fn registering_persists_durably_with_context_intact() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("mock/test").unwrap();
        let context = ctx();
        context.set_session(&session);

        let out = tool(events.clone())
            .execute(
                &context,
                json!({
                    "prompt": "check the deploy",
                    "context": "deploy id: abc123",
                    "in_secs": 600,
                }),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);

        let pending = events.pending_wakeups(&session).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].prompt, "check the deploy");
        assert_eq!(pending[0].context.as_deref(), Some("deploy id: abc123"));
    }

    /// A-98: `in_secs` beyond the configured maximum horizon is refused as a tool-level error, not
    /// silently clamped or dispatched anyway.
    #[tokio::test]
    async fn horizon_beyond_the_configured_maximum_is_refused() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("mock/test").unwrap();
        let context = ctx();
        context.set_session(&session);

        let t = WakeupTool::new(events.clone(), 3600, DEFAULT_MAX_PENDING_PER_SESSION);
        let out = t
            .execute(
                &context,
                json!({ "prompt": "too far out", "in_secs": 7200 }),
            )
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("maximum horizon"), "{}", out.content);
        assert!(events.pending_wakeups(&session).unwrap().is_empty());
    }

    /// A-98: a zero horizon is refused too — "wake me immediately" isn't a wake-up.
    #[tokio::test]
    async fn zero_horizon_is_refused() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("mock/test").unwrap();
        let context = ctx();
        context.set_session(&session);

        let out = tool(events)
            .execute(&context, json!({ "prompt": "now", "in_secs": 0 }))
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.content);
    }

    /// A-98 policy bound: the per-session cap refuses the (N+1)th registration — configurable and
    /// tested, independent of the (also present) approval gate.
    #[tokio::test]
    async fn per_session_cap_refuses_once_reached() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("mock/test").unwrap();
        let context = ctx();
        context.set_session(&session);

        let t = WakeupTool::new(events.clone(), DEFAULT_MAX_HORIZON_SECS, 2);
        for i in 0..2 {
            let out = t
                .execute(
                    &context,
                    json!({ "prompt": format!("wake {i}"), "in_secs": 60 }),
                )
                .await
                .unwrap();
            assert!(!out.is_error, "{}", out.content);
        }
        let refused = t
            .execute(&context, json!({ "prompt": "one too many", "in_secs": 60 }))
            .await
            .unwrap();
        assert!(refused.is_error);
        assert!(refused.content.contains("cap"), "{}", refused.content);
        assert_eq!(events.pending_wakeups(&session).unwrap().len(), 2);
    }

    /// A-98: a call outside any session (no `session_id` on the tool context) is a clear error,
    /// not a silently unscoped registration.
    #[tokio::test]
    async fn no_active_session_is_a_clear_error() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let out = tool(events)
            .execute(&ctx(), json!({ "prompt": "wake", "in_secs": 60 }))
            .await;
        assert!(out.is_err());
    }

    // --- service_due_wakeups_on_open: fires through the EXISTING turn path -------------------

    struct ScriptedProvider {
        responses: Mutex<VecDeque<Vec<Chunk>>>,
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        fn name(&self) -> &str {
            "scripted"
        }
        async fn stream(&self, _request: Request) -> Result<ChunkStream> {
            let chunks = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    fn native_call(id: &str, name: &str, input: Value) -> Vec<Chunk> {
        vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: id.into(),
                name: name.into(),
                input,
            }),
            Chunk::Done {
                stop_reason: Some(StopReason::ToolUse),
            },
        ]
    }

    fn prose(text: &str) -> Vec<Chunk> {
        vec![
            Chunk::TextDelta(text.into()),
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            },
        ]
    }

    /// A minimal chat-only scripted turn: `detect_intent` declares a plain "answer" intent, then
    /// the model answers in prose — the shortest path through the shipped `agent-loop.flux` that
    /// needs no tool other than the built-ins already registered.
    fn chat_responses(answer: &str) -> Vec<Vec<Chunk>> {
        vec![
            native_call(
                "intent-1",
                "declare_intent",
                json!({"intent": "answer", "capability_families": ["core"]}),
            ),
            prose(answer),
        ]
    }

    /// A self-contained `FlowEngine` test harness (mirrors `engine::tests::assemble_test_engine`,
    /// duplicated here rather than shared — that module's helper is `fn`-private to its own test
    /// module and engine.rs is a large, independently-evolving file this story does not touch).
    fn assemble_test_engine(responses: Vec<Vec<Chunk>>, events: Arc<EventStore>) -> FlowEngine {
        let sequence = TEST_ROOT.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "flux-flow-wakeup-engine-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut registry = ToolRegistry::new();
        flux_tools::register_reflect(&mut registry);
        flux_tools::register_evidence(&mut registry);
        let executor = flux_runtime::Executor::new(
            registry,
            PermissionManager::new(),
            Arc::new(AllowApprover),
            RuntimeToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap()))),
        );
        let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let provider: Arc<dyn Provider> = Arc::new(ScriptedProvider {
            responses: Mutex::new(responses.into()),
        });
        FlowEngine::assemble_with_loop(
            provider,
            executor,
            events,
            flow,
            "test-model".into(),
            "Use only observed evidence.".into(),
            2_048,
            5,
            Vec::new(),
            0,
            Vec::new(),
            root,
            AgentLoopSpec::default(),
        )
        .expect("test engine assembly")
    }

    /// A-98 acceptance: a due wake-up fires through the EXISTING turn path — `run_turn`, not a
    /// bespoke route — so it gets a REAL turn id and closes with `TurnEnded` (the C-26 lesson).
    /// The composed input carries the prompt AND the captured context.
    #[tokio::test]
    async fn due_wakeup_fires_as_a_real_turn_with_context_intact() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("scripted/test-model").unwrap();
        events
            .schedule_wakeup(
                &session,
                now_ms() - 1_000, // already due
                "check the deploy",
                Some("deploy id: abc123"),
            )
            .unwrap();

        let engine = assemble_test_engine(chat_responses("deploy looks healthy"), events.clone());
        let mut sink = NullSink;
        let mut lines = Vec::new();
        let fired =
            service_due_wakeups_on_open(&engine, &session, &mut sink, |l| lines.push(l)).await;
        assert_eq!(fired, 1);
        assert!(events.pending_wakeups(&session).unwrap().is_empty());

        let turns = events.turns(&session).unwrap();
        assert_eq!(turns.len(), 1, "the wake-up ran as a real, closed turn");
        let turn = &turns[0];
        assert!(
            turn.turn_id >= 0,
            "a real turn id, never -1 (the C-26 lesson)"
        );
        assert!(turn.ended_at_ms.is_some(), "the turn closed durably");
        assert!(
            turn.user_input.contains("check the deploy"),
            "{}",
            turn.user_input
        );
        assert!(
            turn.user_input.contains("deploy id: abc123"),
            "captured context rehydrates into the fired turn's input: {}",
            turn.user_input
        );
        assert_eq!(turn.answer.as_deref(), Some("deploy looks healthy"));

        assert!(
            lines
                .iter()
                .any(|l| matches!(l, WakeupLine::Info(msg) if msg.contains("firing"))),
            "the firing step must report loudly, never silently"
        );
    }

    /// A-98 cost attribution: the fired turn's `CallUsage`/`TurnEnded` usage lands under the
    /// originating session because firing IS an ordinary turn on that session — `flux usage`
    /// (backed by `cost_summary`) needs no new code to attribute it.
    #[tokio::test]
    async fn fired_wakeup_usage_lands_under_the_originating_session() {
        use flux_core::Usage;

        struct UsageProvider;
        #[async_trait]
        impl Provider for UsageProvider {
            fn name(&self) -> &str {
                "scripted"
            }
            async fn stream(&self, _req: Request) -> Result<ChunkStream> {
                static CALLS: AtomicU64 = AtomicU64::new(0);
                let n = CALLS.fetch_add(1, Ordering::SeqCst);
                let chunks = if n == 0 {
                    let mut c = native_call(
                        "intent-1",
                        "declare_intent",
                        json!({"intent": "answer", "capability_families": ["core"]}),
                    );
                    c.push(Chunk::Usage(Usage {
                        input_tokens: 10,
                        output_tokens: 5,
                        ..Default::default()
                    }));
                    c
                } else {
                    let mut c = prose("done");
                    c.push(Chunk::Usage(Usage {
                        input_tokens: 20,
                        output_tokens: 8,
                        ..Default::default()
                    }));
                    c
                };
                Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
            }
        }

        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("scripted/test-model").unwrap();
        events
            .schedule_wakeup(&session, now_ms() - 1_000, "check it", None)
            .unwrap();

        let root = std::env::temp_dir().join(format!(
            "flux-flow-wakeup-usage-{}-{}",
            std::process::id(),
            TEST_ROOT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut registry = ToolRegistry::new();
        flux_tools::register_reflect(&mut registry);
        flux_tools::register_evidence(&mut registry);
        let executor = flux_runtime::Executor::new(
            registry,
            PermissionManager::new(),
            Arc::new(AllowApprover),
            RuntimeToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap()))),
        );
        let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let engine = FlowEngine::assemble_with_loop(
            Arc::new(UsageProvider),
            executor,
            events.clone(),
            flow,
            "test-model".into(),
            "Use only observed evidence.".into(),
            2_048,
            5,
            Vec::new(),
            0,
            Vec::new(),
            root,
            AgentLoopSpec::default(),
        )
        .unwrap();

        let mut sink = NullSink;
        let fired = service_due_wakeups_on_open(&engine, &session, &mut sink, |_| {}).await;
        assert_eq!(fired, 1);

        let pricing = flux_core::PricingTable::builtin();
        let cost = events.cost_summary(&session, &pricing).unwrap();
        assert!(
            !cost.is_empty(),
            "the fired turn's usage must be attributed under its own (originating) session"
        );
        let total_input: u64 = cost.iter().map(|row| row.usage.input_tokens).sum();
        assert_eq!(total_input, 30, "10 (intent) + 20 (answer) input tokens");
    }

    /// A-98: nothing fires when no wake-up is due yet (a future `fire_at_ms`) — `run_turn` is
    /// never called, and the pending set is untouched.
    #[tokio::test]
    async fn not_yet_due_wakeup_does_not_fire() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("scripted/test-model").unwrap();
        events
            .schedule_wakeup(&session, now_ms() + 3_600_000, "not yet", None)
            .unwrap();

        let engine = assemble_test_engine(chat_responses("should never run"), events.clone());
        let mut sink = NullSink;
        let fired = service_due_wakeups_on_open(&engine, &session, &mut sink, |_| {}).await;
        assert_eq!(fired, 0);
        assert_eq!(events.pending_wakeups(&session).unwrap().len(), 1);
        assert!(events.turns(&session).unwrap().is_empty());
    }
}
