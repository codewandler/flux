//! D-176: Tune's world-pinned re-plan and counterfactual A/B — `Session::what_if()`,
//! `Counterfactual::hermetic`/`cost`, and `Client::what_if_over`.
//!
//! Not feature-gated: `whatif` is un-gated in `lib.rs` (both doors — fork-based and D-176's
//! world-pinned — share the same `Counterfactual`), so this file compiles/runs on every default
//! build, mirroring `tests/agent_test_kit.rs`'s pattern otherwise.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use flux_core::{ContentBlock, Result, StopReason};
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::tools::{Tool, ToolContext, ToolResult, ToolSpec};
use flux_sdk::{Client, Storage};
use serde_json::json;

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("flux-sdk-whatif-{tag}-{}-{n}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn request_has_tool(req: &Request, name: &str) -> bool {
    req.tools.iter().any(|t| t.name == name)
}

fn intent_chunks(intent: &str, families: &[&str]) -> Vec<flux_core::Chunk> {
    vec![
        flux_core::Chunk::Block(ContentBlock::ToolUse {
            id: "intent".into(),
            name: "declare_intent".into(),
            input: json!({ "intent": intent, "capability_families": families }),
        }),
        flux_core::Chunk::Done {
            stop_reason: Some(StopReason::ToolUse),
        },
    ]
}

fn native_call(id: &str, name: &str, input: serde_json::Value) -> Vec<flux_core::Chunk> {
    vec![
        flux_core::Chunk::Block(ContentBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
        }),
        flux_core::Chunk::Done {
            stop_reason: Some(StopReason::ToolUse),
        },
    ]
}

fn prose_chunks(text: &str) -> Vec<flux_core::Chunk> {
    vec![
        flux_core::Chunk::Block(ContentBlock::Text { text: text.into() }),
        flux_core::Chunk::Done {
            stop_reason: Some(StopReason::EndTurn),
        },
    ]
}

fn chunk_stream(chunks: Vec<flux_core::Chunk>) -> ChunkStream {
    Box::pin(futures::stream::iter(chunks.into_iter().map(Ok)))
}

/// A side-effecting op: bumps a shared counter and returns its new value.
struct CounterTool {
    name: &'static str,
    counter: Arc<AtomicUsize>,
}
#[async_trait]
impl Tool for CounterTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            self.name,
            "bump a counter",
            json!({"type": "object", "properties": {}}),
        )
    }
    async fn execute(&self, _c: &ToolContext, _params: serde_json::Value) -> Result<ToolResult> {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(ToolResult::ok(format!("count={n}")))
    }
}

/// Declares intent, then calls `ops` in order (one native call per provider round), then answers
/// with `answer` in prose. Records a single turn — used to build the golden session under test.
struct ScriptedMock {
    ops: Vec<(&'static str, serde_json::Value)>,
    answer: &'static str,
    calls: AtomicUsize,
    /// Total `stream()` invocations (including the intent-declare round) — exposed so a test can
    /// prove a subsequent `what_if` run (reusing this SAME provider `Arc`) never called it again.
    stream_calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Provider for ScriptedMock {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, req: Request) -> Result<ChunkStream> {
        self.stream_calls.fetch_add(1, Ordering::SeqCst);
        if request_has_tool(&req, "declare_intent") {
            return Ok(chunk_stream(intent_chunks("perform the ops", &["core"])));
        }
        let n = self.calls.fetch_add(1, Ordering::Relaxed);
        if let Some((op, input)) = self.ops.get(n) {
            return Ok(chunk_stream(native_call(
                &format!("call-{n}"),
                op,
                input.clone(),
            )));
        }
        Ok(chunk_stream(prose_chunks(self.answer)))
    }
}

/// Branches on whether the REQUEST's system prompt contains `alt_marker` — same call-phase
/// structure (intent → one native op → prose) on either branch, but a DIFFERENT op on the alt
/// branch, so a `.system_prompt(alt)` re-plan genuinely diverges. Keyed per-system-text call count
/// (not a single global counter) so the original recording and a same-provider counterfactual
/// re-plan never interfere with each other's phase tracking.
struct AltPlanMock {
    alt_marker: &'static str,
    calls_by_system: Mutex<std::collections::HashMap<String, usize>>,
}
impl AltPlanMock {
    fn new(alt_marker: &'static str) -> Self {
        Self {
            alt_marker,
            calls_by_system: Mutex::new(std::collections::HashMap::new()),
        }
    }
}
fn full_system_text(req: &Request) -> String {
    let mut s = req.system.clone().unwrap_or_default();
    for seg in &req.system_segments {
        s.push('\n');
        s.push_str(&seg.text);
    }
    s
}
#[async_trait]
impl Provider for AltPlanMock {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, req: Request) -> Result<ChunkStream> {
        if request_has_tool(&req, "declare_intent") {
            return Ok(chunk_stream(intent_chunks("perform the ops", &["core"])));
        }
        let system = full_system_text(&req);
        let alt = system.contains(self.alt_marker);
        let call_index = {
            let mut map = self.calls_by_system.lock().unwrap();
            let n = map.entry(system).or_insert(0);
            let idx = *n;
            *n += 1;
            idx
        };
        if alt {
            match call_index {
                0 => Ok(chunk_stream(native_call(
                    "call-0",
                    "auditlog",
                    json!({"note": "alt"}),
                ))),
                _ => Ok(chunk_stream(prose_chunks("alt answer"))),
            }
        } else {
            match call_index {
                0 => Ok(chunk_stream(native_call("call-0", "bump", json!({})))),
                _ => Ok(chunk_stream(prose_chunks("orig answer"))),
            }
        }
    }
}

fn record_client(
    dir: &std::path::Path,
    ops: Vec<(&'static str, serde_json::Value)>,
    answer: &'static str,
    bump_counter: Arc<AtomicUsize>,
    audit_counter: Arc<AtomicUsize>,
) -> Client {
    record_client_with_stream_calls(dir, ops, answer, bump_counter, audit_counter).0
}

/// Same as [`record_client`], but also returns the provider's total `stream()` call counter — for
/// tests that need to prove a subsequent operation made no further model calls.
fn record_client_with_stream_calls(
    dir: &std::path::Path,
    ops: Vec<(&'static str, serde_json::Value)>,
    answer: &'static str,
    bump_counter: Arc<AtomicUsize>,
    audit_counter: Arc<AtomicUsize>,
) -> (Client, Arc<AtomicUsize>) {
    let stream_calls = Arc::new(AtomicUsize::new(0));
    let client = Client::builder()
        .model("mock")
        .auto_approve(true)
        .storage(Storage::in_memory())
        .register_op(Arc::new(CounterTool {
            name: "bump",
            counter: bump_counter,
        }))
        .register_op(Arc::new(CounterTool {
            name: "auditlog",
            counter: audit_counter,
        }))
        .build(
            Box::new(ScriptedMock {
                ops,
                answer,
                calls: AtomicUsize::new(0),
                stream_calls: stream_calls.clone(),
            }),
            dir,
        )
        .unwrap();
    (client, stream_calls)
}

// --- acceptance tests --------------------------------------------------------

/// Failing-first headline: `.substitute(op, value)` re-runs the identical recorded plan with
/// **zero model calls** (the SAME provider `Arc` the recording used never sees another `stream()`
/// call — `rerun_pinned` cannot invoke the model at all, by construction), `diff()` shows exactly
/// the intended `Output` row at the substituted op, and `hermetic() == true`.
#[tokio::test]
async fn pure_substitution_is_hermetic_and_makes_zero_model_calls() {
    let dir = tmp_dir("pure-sub");
    let bump = Arc::new(AtomicUsize::new(0));
    let audit = Arc::new(AtomicUsize::new(0));
    let (record, stream_calls) = record_client_with_stream_calls(
        &dir,
        vec![
            ("bump", json!({})),
            ("auditlog", json!({"note": "first"})),
            ("bump", json!({})),
        ],
        "done",
        bump,
        audit,
    );
    record.run("do three ops").await.unwrap();
    let calls_after_recording = stream_calls.load(Ordering::SeqCst);
    assert!(
        calls_after_recording > 0,
        "the recording itself called the model"
    );
    let src = record.session_id().unwrap();
    let session = record.open_session(&src).unwrap();

    let cf = session
        .what_if()
        .substitute("auditlog", json!("substituted"))
        .run()
        .await
        .unwrap();

    assert_eq!(
        stream_calls.load(Ordering::SeqCst),
        calls_after_recording,
        "a pure substitution must make ZERO further model calls"
    );
    assert!(cf.hermetic(), "a pure substitution never leaves the tape");

    let diff = cf.diff().unwrap();
    assert!(!diff.identical);
    let output_rows: Vec<_> = diff
        .rows
        .iter()
        .filter(|r| matches!(r, flux_events::DiffRow::Output { .. }))
        .collect();
    assert_eq!(
        output_rows.len(),
        1,
        "exactly one Output row for the substituted op: {diff:?}"
    );
    match output_rows[0] {
        flux_events::DiffRow::Output { op, b, .. } => {
            assert_eq!(op, "auditlog");
            assert_eq!(b, "substituted");
        }
        _ => unreachable!(),
    }
    let plan_rows = diff
        .rows
        .iter()
        .filter(|r| matches!(r, flux_events::DiffRow::Plan { .. }))
        .count();
    assert_eq!(plan_rows, 0, "the plan itself is unchanged: {diff:?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// The counterfactual session produced by a pure substitution is itself a real, hermetically
/// replayable session.
#[tokio::test]
async fn counterfactual_session_is_itself_replayable() {
    let dir = tmp_dir("cf-replayable");
    let record = record_client(
        &dir,
        vec![("bump", json!({}))],
        "bumped",
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    record.run("bump once").await.unwrap();
    let src = record.session_id().unwrap();
    let session = record.open_session(&src).unwrap();

    let cf = session
        .what_if()
        .substitute("bump", json!("replaced"))
        .run()
        .await
        .unwrap();

    struct NullSink;
    impl flux_sdk::AgentSink for NullSink {}
    let mut sink = NullSink;
    let report = cf.session().replay(None, &mut sink).await.unwrap();
    assert!(
        report.diverged.is_none(),
        "the counterfactual's own recorded run must itself replay faithfully: {:?}",
        report.diverged
    );
    assert_eq!(report.cells_consumed, report.cells_total);

    std::fs::remove_dir_all(&dir).ok();
}

/// A pure substitution costs exactly `$0` — no model call was ever made.
#[tokio::test]
async fn pure_substitution_costs_zero() {
    let dir = tmp_dir("cf-zero-cost");
    let record = record_client(
        &dir,
        vec![("bump", json!({}))],
        "bumped",
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    record.run("bump once").await.unwrap();
    let src = record.session_id().unwrap();
    let session = record.open_session(&src).unwrap();

    let cf = session
        .what_if()
        .substitute("bump", json!("replaced"))
        .run()
        .await
        .unwrap();

    let rows = cf.cost(&flux_sdk::PricingTable::builtin()).unwrap();
    let total: f64 = rows.iter().filter_map(|r| r.cost.map(|m| m.usd)).sum();
    assert_eq!(total, 0.0, "no model call was ever made: {rows:?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// A `.system_prompt()` re-plan variant is honestly reported as non-hermetic, with the first
/// divergence localized to the statement whose op actually changed — never a faked complete diff.
#[tokio::test]
async fn system_prompt_replan_is_non_hermetic_with_a_localized_divergence() {
    let dir = tmp_dir("replan-divergence");
    let bump = Arc::new(AtomicUsize::new(0));
    let audit = Arc::new(AtomicUsize::new(0));
    let alt_marker = "USE_THE_ALT_PLAN";
    let client = Client::builder()
        .model("mock")
        .auto_approve(true)
        .storage(Storage::in_memory())
        .register_op(Arc::new(CounterTool {
            name: "bump",
            counter: bump,
        }))
        .register_op(Arc::new(CounterTool {
            name: "auditlog",
            counter: audit,
        }))
        .build(Box::new(AltPlanMock::new(alt_marker)), &dir)
        .unwrap();

    client.run("go").await.unwrap();
    let src = client.session_id().unwrap();
    let session = client.open_session(&src).unwrap();

    let cf = session
        .what_if()
        .system_prompt(format!("You are a helpful agent. {alt_marker}"))
        .run()
        .await
        .unwrap();

    assert!(
        !cf.hermetic(),
        "a re-plan that reads a different system prompt must leave the pinned world"
    );
    let divergence = cf
        .first_divergence()
        .expect("the alternate plan calls a different op at the same position");
    assert_eq!(
        divergence.node, 0,
        "divergence localized at the first statement: {divergence:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `Scenario`-free `off_tape(Halt)` latches loudly the moment a re-plan's dispatch has no matching
/// recorded cell (rather than silently falling through), and that latch is exactly why the
/// counterfactual is honestly reported non-hermetic.
#[tokio::test]
async fn off_tape_halt_latches_loudly_on_the_replan_path() {
    let dir = tmp_dir("off-tape-halt");
    let bump = Arc::new(AtomicUsize::new(0));
    let audit = Arc::new(AtomicUsize::new(0));
    let alt_marker = "DIVERGE_HERE";
    let client = Client::builder()
        .model("mock")
        .auto_approve(true)
        .storage(Storage::in_memory())
        .register_op(Arc::new(CounterTool {
            name: "bump",
            counter: bump,
        }))
        .register_op(Arc::new(CounterTool {
            name: "auditlog",
            counter: audit,
        }))
        .build(Box::new(AltPlanMock::new(alt_marker)), &dir)
        .unwrap();
    client.run("go").await.unwrap();
    let src = client.session_id().unwrap();
    let session = client.open_session(&src).unwrap();

    let cf = session
        .what_if()
        .system_prompt(format!("You are a helpful agent. {alt_marker}"))
        .off_tape(flux_sdk::whatif::OffTape::Halt)
        .run()
        .await
        .unwrap();

    assert!(!cf.hermetic());

    std::fs::remove_dir_all(&dir).ok();
}

/// `Client::what_if_over` sweeps several sessions under one spec; a session that can't be opened
/// lands as an isolated `Err` row rather than aborting the whole sweep.
#[tokio::test]
async fn what_if_over_isolates_a_broken_session_in_the_sweep() {
    let dir = tmp_dir("sweep");
    let client = record_client(
        &dir,
        vec![("bump", json!({}))],
        "bumped",
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    client.run("bump once").await.unwrap();
    let good = client.session_id().unwrap();

    let good_session = client.open_session(&good).unwrap();
    let spec = good_session
        .what_if()
        .substitute("bump", json!("swept"))
        .spec();
    let report = client
        .what_if_over(vec![good.clone(), "does-not-exist".to_string()], spec)
        .await
        .unwrap();

    assert_eq!(report.total, 2);
    assert_eq!(report.outcomes.len(), 2);
    assert!(
        report.outcomes[0].result.is_ok(),
        "the good session succeeds"
    );
    assert!(
        report.outcomes[1].result.is_err(),
        "the missing session is isolated as an Err row, not a sweep abort"
    );
    assert_eq!(
        report.changed, 2,
        "the good session diverged (substitution) and the broken one counts as changed too"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// --- D-177: policy mode ------------------------------------------------------

/// Failing-first: under a STRICTER policy, an op the recording ran is denied — and the denial
/// surfaces as a real divergence rather than being masked by the taped output.
#[tokio::test]
async fn a_stricter_policy_denies_an_op_the_recording_ran() {
    let dir = tmp_dir("policy-strict");
    let record = record_client(
        &dir,
        vec![("bump", json!({})), ("auditlog", json!({"n": 1}))],
        "did both",
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    record.run("do both").await.unwrap();
    let src = record.session_id().unwrap();
    let session = record.open_session(&src).unwrap();

    let cf = session
        .what_if()
        .policy(flux_sdk::Permissions {
            allow: vec!["bump".into()],
            deny: vec!["auditlog".into()],
        })
        .run()
        .await
        .unwrap();

    assert!(
        !cf.hermetic(),
        "a policy denial means the run left the recorded world — that IS the answer"
    );
    let trace = cf.session().run_trace().unwrap();
    // The refused op's RECORDED OUTCOME is the real refusal, not the taped output it would have
    // been served under the original policy.
    let refused: Vec<_> = trace
        .iter()
        .filter_map(|ev| match ev {
            flux_lang::ast::RunEvent::OpRecorded {
                op,
                content,
                is_error,
                ..
            } if *is_error => Some((op.clone(), content.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(refused.len(), 1, "exactly the refused op: {trace:?}");
    assert_eq!(refused[0].0, "auditlog");
    assert!(
        refused[0].1.contains("denied by permission rules"),
        "the recorded outcome is the real refusal, not the taped output: {}",
        refused[0].1
    );
    // ...and the plan halted on it as a DENIAL specifically — the interpreter's fatal, never-retried
    // classification, not a generic op failure a `retry` could paper over.
    let halted = trace.iter().any(|ev| {
        matches!(
            ev,
            flux_lang::ast::RunEvent::PlanHalted { op: Some(op), kind, .. }
                if op == "auditlog" && matches!(kind, flux_lang::ast::FailureKind::Denied)
        )
    });
    assert!(halted, "the denial must halt the plan as Denied: {trace:?}");
    // The op that the stricter policy still allows ran (was served) exactly as recorded.
    assert!(trace.iter().any(|ev| matches!(
        ev,
        flux_lang::ast::RunEvent::OpRecorded { op, content, .. } if op == "bump" && content == "count=1"
    )));

    std::fs::remove_dir_all(&dir).ok();
}

/// Under an EQUAL (or looser) policy nothing changes: every op is still served from the frozen tape,
/// the run stays hermetic, and the diff is identical.
#[tokio::test]
async fn an_equal_policy_stays_hermetic_and_serves_from_tape() {
    let dir = tmp_dir("policy-equal");
    let live = Arc::new(AtomicUsize::new(0));
    let record = record_client(
        &dir,
        vec![("bump", json!({}))],
        "bumped",
        live.clone(),
        Arc::new(AtomicUsize::new(0)),
    );
    record.run("bump once").await.unwrap();
    let calls_after_recording = live.load(Ordering::SeqCst);
    let src = record.session_id().unwrap();
    let session = record.open_session(&src).unwrap();

    let cf = session
        .what_if()
        .policy(flux_sdk::Permissions {
            allow: vec!["bump".into(), "auditlog".into()],
            deny: vec![],
        })
        .run()
        .await
        .unwrap();

    assert!(
        cf.hermetic(),
        "an equally-permissive policy admits every recorded op — nothing left the tape"
    );
    assert_eq!(
        live.load(Ordering::SeqCst),
        calls_after_recording,
        "re-authorizing must not re-run the op: it is still served from the frozen world"
    );
    assert!(cf.diff().unwrap().identical, "{:?}", cf.diff().unwrap());

    std::fs::remove_dir_all(&dir).ok();
}

// --- D-182: re-plan path self-records served hits ----------------------------

/// Failing-first (D-182): a `.system_prompt()` re-plan whose new plan happens to be BYTE-IDENTICAL
/// to the original — same op, same input — must be fully served from the frozen tape and diff as
/// `identical`, not as a fake total divergence. Before the fix, the re-plan path drove
/// `run_turn_pinned` with a bare `NullSink`: a served (non-live) dispatch is never recorded by the
/// cassette layer itself (nothing ran live, so there is no live tail to record), so the
/// counterfactual session's own trace ended up with ZERO cells even though one statement executed —
/// `run_diff` then reads "no cells on the b side" as every statement having vanished.
#[tokio::test]
async fn replan_with_an_identical_plan_is_fully_served_and_diffs_identical() {
    let dir = tmp_dir("replan-identical");
    let bump = Arc::new(AtomicUsize::new(0));
    let audit = Arc::new(AtomicUsize::new(0));
    // Any marker not present in either system prompt below keeps `AltPlanMock` on its "bump" branch
    // for BOTH the original recording and the counterfactual re-plan.
    let alt_marker = "NEVER_PRESENT_MARKER";
    let client = Client::builder()
        .model("mock")
        .auto_approve(true)
        .storage(Storage::in_memory())
        .register_op(Arc::new(CounterTool {
            name: "bump",
            counter: bump,
        }))
        .register_op(Arc::new(CounterTool {
            name: "auditlog",
            counter: audit,
        }))
        .build(Box::new(AltPlanMock::new(alt_marker)), &dir)
        .unwrap();

    client.run("go").await.unwrap();
    let src = client.session_id().unwrap();
    let session = client.open_session(&src).unwrap();

    let cf = session
        .what_if()
        .system_prompt("You are a differently-worded but non-alt agent.")
        .run()
        .await
        .unwrap();

    assert!(
        cf.hermetic(),
        "the re-plan called the identical op with the identical input — nothing left the tape"
    );
    let diff = cf.diff().unwrap();
    assert!(
        diff.identical,
        "a fully-served, identical re-plan must diff as identical, not a fake divergence: {diff:?}"
    );

    // The counterfactual session's own trace must actually hold the served cell — the whole point
    // of self-recording (an empty trace would ALSO make `diff.identical` look true by accident, if
    // `run_diff` treated two empty sides as equal — assert directly that the cell exists).
    let cf_trace = cf.session().run_trace().unwrap();
    let recorded_ops: Vec<_> = cf_trace
        .iter()
        .filter_map(|ev| match ev {
            flux_lang::ast::RunEvent::OpRecorded { op, .. } => Some(op.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        recorded_ops,
        vec!["bump"],
        "the served dispatch must be self-recorded onto the counterfactual session: {cf_trace:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A re-plan whose new plan diverges — calling an EXTRA op the original recording never made — under
/// `.off_tape(Live)` must record BOTH the served hit (the op the original plan also called) and the
/// live fall-through (the new op) onto the counterfactual's own trace, not just the live tail.
/// Before the D-182 fix, `off_tape(Live)`'s bridge only ever recorded the live-miss tail
/// (`FrozenTape::record_tail`) — a served hit sitting right next to it went unrecorded, so the
/// counterfactual's trace was missing statements the diff needed to align against.
struct MixedReplanMock {
    alt_marker: &'static str,
    calls_by_system: Mutex<std::collections::HashMap<String, usize>>,
}
impl MixedReplanMock {
    fn new(alt_marker: &'static str) -> Self {
        Self {
            alt_marker,
            calls_by_system: Mutex::new(std::collections::HashMap::new()),
        }
    }
}
#[async_trait]
impl Provider for MixedReplanMock {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, req: Request) -> Result<ChunkStream> {
        if request_has_tool(&req, "declare_intent") {
            return Ok(chunk_stream(intent_chunks("perform the ops", &["core"])));
        }
        let system = full_system_text(&req);
        let alt = system.contains(self.alt_marker);
        let call_index = {
            let mut map = self.calls_by_system.lock().unwrap();
            let n = map.entry(system).or_insert(0);
            let idx = *n;
            *n += 1;
            idx
        };
        if alt {
            // Same first op as the original (served from tape), PLUS a second op the original
            // recording never called at all (a live miss under `OffTape::Live`).
            match call_index {
                0 => Ok(chunk_stream(native_call("call-0", "bump", json!({})))),
                1 => Ok(chunk_stream(native_call(
                    "call-1",
                    "auditlog",
                    json!({"note": "new"}),
                ))),
                _ => Ok(chunk_stream(prose_chunks("alt answer"))),
            }
        } else {
            match call_index {
                0 => Ok(chunk_stream(native_call("call-0", "bump", json!({})))),
                _ => Ok(chunk_stream(prose_chunks("orig answer"))),
            }
        }
    }
}

#[tokio::test]
async fn off_tape_live_replan_records_both_served_and_live_cells() {
    let dir = tmp_dir("replan-mixed-live");
    let bump = Arc::new(AtomicUsize::new(0));
    let audit = Arc::new(AtomicUsize::new(0));
    let alt_marker = "MIXED_LIVE_MARKER";
    let client = Client::builder()
        .model("mock")
        .auto_approve(true)
        .storage(Storage::in_memory())
        .register_op(Arc::new(CounterTool {
            name: "bump",
            counter: bump,
        }))
        .register_op(Arc::new(CounterTool {
            name: "auditlog",
            counter: audit,
        }))
        .build(Box::new(MixedReplanMock::new(alt_marker)), &dir)
        .unwrap();

    client.run("go").await.unwrap();
    let src = client.session_id().unwrap();
    let session = client.open_session(&src).unwrap();

    let cf = session
        .what_if()
        .system_prompt(format!("You are a helpful agent. {alt_marker}"))
        .off_tape(flux_sdk::whatif::OffTape::Live)
        .run()
        .await
        .unwrap();

    assert!(
        !cf.hermetic(),
        "the new plan's extra op has no matching recorded cell — it must go live"
    );

    let cf_trace = cf.session().run_trace().unwrap();
    let mut recorded_ops: Vec<_> = cf_trace
        .iter()
        .filter_map(|ev| match ev {
            flux_lang::ast::RunEvent::OpRecorded { op, .. } => Some(op.clone()),
            _ => None,
        })
        .collect();
    recorded_ops.sort();
    // Completeness, not positional order: the re-plan drives the FULL adaptive agent loop (a
    // composite, `explore`), whose own dispatch relay delivers this sink's `tool_call`/`tool_result`
    // pairs asynchronously relative to `FrozenTape::record_tail`'s SYNCHRONOUS live-bridge write —
    // so a self-recorded served cell can land at a different STREAM position than it would in true
    // execution order (a known, narrower limitation than full positional `diff()` alignment,
    // documented at `RerunRecordingSink::defer_for_live_bridge`/`finish`). What matters here is that
    // NEITHER cell goes missing NOR gets duplicated.
    assert_eq!(
        recorded_ops,
        vec!["auditlog".to_string(), "bump".to_string()],
        "both the SERVED hit (bump) and the LIVE fall-through (auditlog) must be recorded onto the \
         counterfactual's own trace, exactly once each — no drop, no duplicate: {cf_trace:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// --- D-182 (handed over from D-184): `substitute_at` on a dead node errors -----

/// Failing-first: `.substitute_at(node, _)` where `node` maps to no recorded dispatch (a typo'd node
/// id, or a node whose statement made no dispatch at all) must return an error naming the node,
/// never silently drop the substitution and report an honest-looking identical run.
#[tokio::test]
async fn substitute_at_a_dead_node_errors_instead_of_silently_no_opping() {
    let dir = tmp_dir("substitute-at-dead-node");
    let record = record_client(
        &dir,
        vec![("bump", json!({}))],
        "bumped",
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    record.run("bump once").await.unwrap();
    let src = record.session_id().unwrap();
    let session = record.open_session(&src).unwrap();

    // A node id nothing in the recorded plan ever bound to — no statement, no dispatch, nothing to
    // substitute.
    let no_such_node = 9_999u32;
    let err = match session
        .what_if()
        .substitute_at(no_such_node, json!("should never be served"))
        .run()
        .await
    {
        Ok(_) => panic!("a dead node target must error, not silently run as if nothing changed"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains(&no_such_node.to_string()),
        "the error must name the offending node: {err}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
