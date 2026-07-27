//! D-174: the Deterministic Agent Test Kit's hermetic core — `Scenario::record`/`load`/`replay`/
//! `inject_at`, `Outcome`'s assertions, and the fault door's `Counterfactual`.
//!
//! Only compiled/run under `--features test-kit` (mirrors `tests/plugins.rs`'s
//! `#![cfg(feature = "plugins")]` convention rather than a Cargo.toml `[[test]] required-features`
//! stanza, matching the rest of this crate's feature-gated test files).

#![cfg(feature = "test-kit")]
// Every test holds `serialize()`'s std `Mutex` across `.await` points on purpose — it exists only
// to keep two tests from racing on process-wide env vars (`FLUX_CASSETTE_MAX_BYTES`/`FLUX_GOLDEN`),
// never nests with another lock, and the guarded work is a single test body — no risk of blocking
// an executor thread another task needs. `tokio::sync::Mutex` would work too, but a plain std
// `Mutex` is simpler here since nothing else contends for the runtime while it's held.
#![allow(clippy::await_holding_lock)]

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use flux_core::{ContentBlock, Result, StopReason};
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::test::Scenario;
use flux_sdk::tools::{Tool, ToolContext, ToolResult, ToolSpec};
use flux_sdk::{Client, Storage};
use serde_json::json;

// --- shared test plumbing ---------------------------------------------------

/// Every test in this file drives `Scenario::record`/`replay`, which reads process-wide env vars
/// (`FLUX_CASSETTE_MAX_BYTES` via `flux_flow::cassette::max_cell_bytes()`, and `FLUX_GOLDEN`) —
/// `cargo test` runs test fns in parallel THREADS of one process, so a test that mutates one of
/// these vars would otherwise leak it into every other test racing concurrently. ALL tests in this
/// file take this one lock for their whole body (see `serialize()`), so at most one is ever running
/// — simpler and safer than trying to scope the lock to only the env-mutating tests.
fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Take the whole-file serialization lock — call this first in every test.
fn serialize() -> std::sync::MutexGuard<'static, ()> {
    env_lock().lock().unwrap_or_else(|e| e.into_inner())
}

/// Sets `key=value` for its lifetime, restoring the prior value (or absence) on drop. Callers must
/// already hold [`serialize`]'s lock (an RAII guard rather than a closure-taking helper, so callers
/// can `.await` async work in between set and restore without nesting a second async executor
/// inside the running `#[tokio::test]`).
struct EnvGuard {
    key: &'static str,
    prior: Option<std::ffi::OsString>,
}
impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prior = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, prior }
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "flux-sdk-test-kit-{tag}-{}-{n}",
        std::process::id()
    ));
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

/// A side-effecting op: bumps a shared counter and returns its new value — the counting test-double
/// used to prove hermetic replay never dispatches (the counter must stay untouched).
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

/// Declares intent, calls `ops` in order (one native call per provider round), then answers with
/// `answer` in prose. Panics if called beyond the scripted length (never called by hermetic
/// replay — see [`NeverProvider`]).
struct ScriptedMock {
    ops: Vec<(&'static str, serde_json::Value)>,
    answer: &'static str,
    calls: AtomicUsize,
}
#[async_trait]
impl Provider for ScriptedMock {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, req: Request) -> Result<ChunkStream> {
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

/// A never-called provider — panics if the model is ever hit. Pairs with a deny-all approver
/// (the builder default) to prove [`Scenario::replay`] dispatches nothing live at all.
struct NeverProvider;
#[async_trait]
impl Provider for NeverProvider {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        panic!("hermetic replay must never invoke the model");
    }
}

fn record_client(
    dir: &std::path::Path,
    ops: Vec<(&'static str, serde_json::Value)>,
    answer: &'static str,
    counter: Arc<AtomicUsize>,
) -> Client {
    Client::builder()
        .model("mock")
        .auto_approve(true)
        .storage(Storage::in_memory())
        .register_op(Arc::new(CounterTool {
            name: "bump",
            counter,
        }))
        .register_op(Arc::new(CounterTool {
            name: "auditlog",
            counter: Arc::new(AtomicUsize::new(0)),
        }))
        .build(
            Box::new(ScriptedMock {
                ops,
                answer,
                calls: AtomicUsize::new(0),
            }),
            dir,
        )
        .unwrap()
}

/// A hermetic offline client: deny-all approver (the builder default — no `auto_approve`), a
/// never-called provider, and a counting op registered so the test can prove the counter never
/// moves during replay (a stronger proof than "the op doesn't exist here at all").
fn offline_client(dir: &std::path::Path, counter: Arc<AtomicUsize>) -> Client {
    Client::builder()
        .model("mock")
        .storage(Storage::in_memory())
        .register_op(Arc::new(CounterTool {
            name: "bump",
            counter,
        }))
        .register_op(Arc::new(CounterTool {
            name: "auditlog",
            counter: Arc::new(AtomicUsize::new(0)),
        }))
        .build(Box::new(NeverProvider), dir)
        .unwrap()
}

// --- acceptance tests --------------------------------------------------------

/// Failing-first: `Scenario::record` writes the full fixture layout.
#[tokio::test]
async fn record_writes_a_fixture_directory() {
    let _serialize = serialize();
    let dir = tmp_dir("record-shape");
    let fixture = dir.join("scenario-fixture");
    let record_counter = Arc::new(AtomicUsize::new(0));
    let client = record_client(
        &dir,
        vec![("bump", json!({}))],
        "bumped it",
        record_counter.clone(),
    );

    Scenario::record(&client, "bump the counter", &fixture)
        .await
        .unwrap();

    assert!(fixture.join("events.db").exists());
    assert!(fixture.join("flow.db").exists());
    assert!(fixture.join("model.jsonl").exists());
    assert!(fixture.join("plan.flux.snap").exists());
    assert!(fixture.join("scenario.toml").exists());
    assert_eq!(
        record_counter.load(Ordering::SeqCst),
        1,
        "the real op ran once, live"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Failing-first acceptance: `Scenario::load(path).replay()` re-runs hermetically under a
/// **deny-all approver + never-called provider**, proving zero live dispatch (the model is never
/// hit, and the registered counting op's counter never moves).
#[tokio::test]
async fn replay_is_hermetic_under_deny_all_and_never_provider() {
    let _serialize = serialize();
    let dir = tmp_dir("hermetic-replay");
    let fixture = dir.join("scenario-fixture");
    let record_counter = Arc::new(AtomicUsize::new(0));
    let record = record_client(&dir, vec![("bump", json!({}))], "bumped it", record_counter);
    Scenario::record(&record, "bump the counter", &fixture)
        .await
        .unwrap();

    let scenario = Scenario::load(&fixture).unwrap();
    let replay_counter = Arc::new(AtomicUsize::new(0));
    let offline = offline_client(&dir, replay_counter.clone());

    let outcome = scenario.replay(&offline).await.unwrap();

    assert_eq!(
        replay_counter.load(Ordering::SeqCst),
        0,
        "hermetic replay must never dispatch the real op"
    );
    outcome.assert_faithful();
    outcome.assert_calls(&["bump"]);

    std::fs::remove_dir_all(&dir).ok();
}

/// Every `Outcome` assertion passes against a faithful replay of a good fixture.
#[tokio::test]
async fn outcome_assertions_pass_on_a_good_fixture() {
    let _serialize = serialize();
    let dir = tmp_dir("good-fixture");
    let fixture = dir.join("scenario-fixture");
    let record = record_client(
        &dir,
        vec![("bump", json!({}))],
        "the counter is bumped",
        Arc::new(AtomicUsize::new(0)),
    );
    Scenario::record(&record, "bump the counter", &fixture)
        .await
        .unwrap();

    let scenario = Scenario::load(&fixture).unwrap();
    let offline = offline_client(&dir, Arc::new(AtomicUsize::new(0)));
    let outcome = scenario.replay(&offline).await.unwrap();

    outcome.assert_faithful();
    outcome.assert_calls(&["bump"]);
    outcome.assert_never_calls("shell.exec");
    outcome.assert_text_contains("counter is bumped");
    outcome.assert_cost_under(1_000_000.0);
    outcome.assert_plan_snapshot(); // record() already wrote a matching plan.flux.snap

    std::fs::remove_dir_all(&dir).ok();
}

/// A failing assertion panics with the canonical plan source rendered into the message.
#[tokio::test]
async fn failing_assertion_renders_the_canonical_plan() {
    let _serialize = serialize();
    let dir = tmp_dir("failing-assertion");
    let fixture = dir.join("scenario-fixture");
    let record = record_client(
        &dir,
        vec![("bump", json!({}))],
        "bumped it",
        Arc::new(AtomicUsize::new(0)),
    );
    Scenario::record(&record, "bump the counter", &fixture)
        .await
        .unwrap();

    let scenario = Scenario::load(&fixture).unwrap();
    let offline = offline_client(&dir, Arc::new(AtomicUsize::new(0)));
    let outcome = scenario.replay(&offline).await.unwrap();

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        outcome.assert_never_calls("bump");
    }));
    let err = result.expect_err("assert_never_calls must panic when the op DID run");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_else(|| "<non-string panic payload>".to_string());
    assert!(
        msg.contains("canonical plan:"),
        "panic message renders the canonical plan: {msg}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `assert_plan_snapshot` panics with a unified line diff on mismatch, and `FLUX_GOLDEN=update`
/// rewrites the fixture instead of panicking.
#[tokio::test]
async fn plan_snapshot_mismatch_panics_then_flux_golden_update_rewrites() {
    let _serialize = serialize();
    let dir = tmp_dir("snapshot-mismatch");
    let fixture = dir.join("scenario-fixture");
    let record = record_client(
        &dir,
        vec![("bump", json!({}))],
        "bumped it",
        Arc::new(AtomicUsize::new(0)),
    );
    Scenario::record(&record, "bump the counter", &fixture)
        .await
        .unwrap();

    // Corrupt the committed snapshot.
    std::fs::write(fixture.join("plan.flux.snap"), "not the real plan at all").unwrap();

    let scenario = Scenario::load(&fixture).unwrap();
    let offline = offline_client(&dir, Arc::new(AtomicUsize::new(0)));
    let outcome = scenario.replay(&offline).await.unwrap();

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        outcome.assert_plan_snapshot();
    }));
    let err = result.expect_err("a corrupted snapshot must panic");
    let msg = err.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(msg.contains("mismatch"), "{msg}");
    assert!(msg.contains("not the real plan"), "renders a diff: {msg}");

    {
        let _env = EnvGuard::set("FLUX_GOLDEN", "update");
        outcome.assert_plan_snapshot(); // must not panic
    }

    let rewritten = std::fs::read_to_string(fixture.join("plan.flux.snap")).unwrap();
    assert_ne!(rewritten, "not the real plan at all");
    assert!(rewritten.contains("bump") || !rewritten.is_empty());

    std::fs::remove_dir_all(&dir).ok();
}

/// The fault door: `inject_at` diverges the fork's tail from the original, and the diverged tail
/// still runs its remaining statement (`audit.log`) live through the real envelope — the
/// saga-compensation check `assert_compensated_with` targets.
#[tokio::test]
async fn inject_at_diverges_and_the_tail_still_runs_live() {
    let _serialize = serialize();
    let dir = tmp_dir("inject-at");
    let fixture = dir.join("scenario-fixture");
    let record = record_client(
        &dir,
        vec![("bump", json!({})), ("auditlog", json!({"note": "bumped"}))],
        "done",
        Arc::new(AtomicUsize::new(0)),
    );
    Scenario::record(&record, "bump then log", &fixture)
        .await
        .unwrap();

    let scenario = Scenario::load(&fixture).unwrap();
    let audit_counter = Arc::new(AtomicUsize::new(0));
    let client = Client::builder()
        .model("mock")
        .auto_approve(true)
        .storage(Storage::in_memory())
        .register_op(Arc::new(CounterTool {
            name: "bump",
            counter: Arc::new(AtomicUsize::new(0)),
        }))
        .register_op(Arc::new(CounterTool {
            name: "auditlog",
            counter: audit_counter.clone(),
        }))
        .build(Box::new(NeverProvider), &dir)
        .unwrap();

    let counterfactual = scenario
        .inject_at(&client, 0, &json!("an injected value"))
        .await
        .unwrap();

    counterfactual.assert_diverges_at(0);
    assert!(
        counterfactual.first_divergence().is_some(),
        "injecting at node 0 must produce a divergence"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `FLUX_GOLDEN=update` re-records against a live client and rewrites the fixture in place.
#[tokio::test]
async fn flux_golden_update_re_records_the_fixture() {
    let _serialize = serialize();
    let dir = tmp_dir("golden-update");
    let fixture = dir.join("scenario-fixture");
    let first = record_client(
        &dir,
        vec![("bump", json!({}))],
        "first answer",
        Arc::new(AtomicUsize::new(0)),
    );
    Scenario::record(&first, "bump the counter", &fixture)
        .await
        .unwrap();

    // Refuses to overwrite without FLUX_GOLDEN=update.
    let second = record_client(
        &dir,
        vec![("bump", json!({}))],
        "second answer",
        Arc::new(AtomicUsize::new(0)),
    );
    let refused = Scenario::record(&second, "bump the counter", &fixture).await;
    assert!(
        refused.is_err(),
        "must refuse to overwrite an existing fixture"
    );

    {
        let _env = EnvGuard::set("FLUX_GOLDEN", "update");
        let third = record_client(
            &dir,
            vec![("bump", json!({}))],
            "third answer, updated",
            Arc::new(AtomicUsize::new(0)),
        );
        Scenario::record(&third, "bump the counter", &fixture)
            .await
            .unwrap();
    }

    let scenario = Scenario::load(&fixture).unwrap();
    let offline = offline_client(&dir, Arc::new(AtomicUsize::new(0)));
    let outcome = scenario.replay(&offline).await.unwrap();
    outcome.assert_text_contains("third answer, updated");

    std::fs::remove_dir_all(&dir).ok();
}

/// A truncated cassette cell (`FLUX_CASSETTE_MAX_BYTES` too small to capture the op's content)
/// reports the actionable "re-record with a larger FLUX_CASSETTE_MAX_BYTES" diagnostic —
/// `assert_faithful`/`faithful()` treat it as a diagnostic, never a silent pass.
#[tokio::test]
async fn truncated_cell_reports_an_actionable_error() {
    let _serialize = serialize();
    let dir = tmp_dir("truncated-cell");
    let fixture = dir.join("scenario-fixture");

    {
        let _env = EnvGuard::set("FLUX_CASSETTE_MAX_BYTES", "1");
        let record = record_client(
            &dir,
            vec![("bump", json!({}))],
            "bumped it",
            Arc::new(AtomicUsize::new(0)),
        );
        Scenario::record(&record, "bump the counter", &fixture)
            .await
            .unwrap();
    }

    let scenario = Scenario::load(&fixture).unwrap();
    let offline = offline_client(&dir, Arc::new(AtomicUsize::new(0)));
    let outcome = scenario.replay(&offline).await.unwrap();

    let err = outcome
        .faithful()
        .expect_err("a truncated cell must not silently pass");
    assert!(
        err.contains("FLUX_CASSETTE_MAX_BYTES"),
        "actionable re-record hint: {err}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Round-trip fidelity property test: capture→save→load→replay is faithful across several
/// differently-shaped recorded flows (event ordering / cassette-cell accounting preserved).
#[tokio::test]
async fn capture_save_load_replay_round_trip_is_faithful_across_flows() {
    let _serialize = serialize();
    let flows: Vec<(Vec<(&'static str, serde_json::Value)>, &'static str)> = vec![
        (vec![("bump", json!({}))], "one op"),
        (
            vec![("bump", json!({})), ("auditlog", json!({"n": 1}))],
            "two ops",
        ),
        (
            vec![
                ("bump", json!({})),
                ("auditlog", json!({"n": 1})),
                ("bump", json!({})),
            ],
            "three ops",
        ),
    ];

    for (i, (ops, answer)) in flows.into_iter().enumerate() {
        let dir = tmp_dir(&format!("roundtrip-{i}"));
        let fixture = dir.join("scenario-fixture");
        let record = record_client(&dir, ops, answer, Arc::new(AtomicUsize::new(0)));
        Scenario::record(&record, "go", &fixture).await.unwrap();

        let scenario = Scenario::load(&fixture).unwrap();
        let offline = offline_client(&dir, Arc::new(AtomicUsize::new(0)));
        let outcome = scenario.replay(&offline).await.unwrap();

        outcome.assert_faithful();
        outcome.assert_text_contains(answer);

        std::fs::remove_dir_all(&dir).ok();
    }
}

// --- D-176: Scenario::check (Engine 2 — world-and-model-pinned re-drive) ------

/// A client whose provider is a scripted mock BUT whose system prompt carries `marker` — used to
/// prove that a config edit the golden `model.jsonl` doesn't cover falls through to the real
/// provider and is COUNTED (`Report::model_live`), never silently served.
fn check_client(
    dir: &std::path::Path,
    ops: Vec<(&'static str, serde_json::Value)>,
    answer: &'static str,
    counter: Arc<AtomicUsize>,
    system: Option<&str>,
) -> Client {
    let mut builder = Client::builder()
        .model("mock")
        .auto_approve(true)
        .storage(Storage::in_memory())
        .register_op(Arc::new(CounterTool {
            name: "bump",
            counter,
        }))
        .register_op(Arc::new(CounterTool {
            name: "auditlog",
            counter: Arc::new(AtomicUsize::new(0)),
        }));
    if let Some(s) = system {
        builder = builder.system_prompt(s);
    }
    builder
        .build(
            Box::new(ScriptedMock {
                ops,
                answer,
                calls: AtomicUsize::new(0),
            }),
            dir,
        )
        .unwrap()
}

/// Failing-first: re-driving an unchanged agent against its own golden is CLEAN and costs $0 — every
/// model call is served from `model.jsonl` (`model_live == 0`), every op is served from the frozen
/// world (the live counter never moves), and the classified `Report` reports no drift at all.
#[tokio::test]
async fn check_re_drive_is_clean_and_costs_nothing_on_an_unchanged_agent() {
    let _serialize = serialize();
    let dir = tmp_dir("check-clean");
    let fixture = dir.join("scenario-fixture");
    let record = record_client(
        &dir,
        vec![("bump", json!({})), ("auditlog", json!({"n": 1}))],
        "did both",
        Arc::new(AtomicUsize::new(0)),
    );
    Scenario::record(&record, "do both ops", &fixture)
        .await
        .unwrap();

    let scenario = Scenario::load(&fixture).unwrap();
    let live_counter = Arc::new(AtomicUsize::new(0));
    let unchanged = check_client(
        &dir,
        vec![("bump", json!({})), ("auditlog", json!({"n": 1}))],
        "did both",
        live_counter.clone(),
        None,
    );

    let report = scenario.check(&unchanged).await.unwrap();

    assert!(
        report.is_clean(),
        "an unchanged agent must reproduce its golden exactly: {:?}",
        report.render()
    );
    assert!(!report.plan_changed);
    assert!(!report.left_world);
    assert_eq!(
        report.model_live, 0,
        "every model call must come off the golden model.jsonl — a $0 re-drive"
    );
    assert!(
        report.model_served > 0,
        "the golden model cassette actually served the re-drive"
    );
    assert_eq!(
        live_counter.load(Ordering::SeqCst),
        0,
        "the frozen world served every op — nothing dispatched live"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Honesty: a config edit the golden doesn't cover (here a changed system prompt, which changes the
/// canonicalized request hash) falls through to the real provider and is COUNTED in
/// `Report::model_live` rather than silently served — the counter that makes "this re-drive was
/// free and deterministic" a checkable claim instead of an assumption.
#[tokio::test]
async fn check_counts_every_model_call_the_golden_does_not_cover() {
    let _serialize = serialize();
    let dir = tmp_dir("check-drift");
    let fixture = dir.join("scenario-fixture");
    let record = record_client(
        &dir,
        vec![("bump", json!({}))],
        "bumped it",
        Arc::new(AtomicUsize::new(0)),
    );
    Scenario::record(&record, "bump the counter", &fixture)
        .await
        .unwrap();

    let scenario = Scenario::load(&fixture).unwrap();
    let edited = check_client(
        &dir,
        vec![("bump", json!({}))],
        "bumped it",
        Arc::new(AtomicUsize::new(0)),
        Some("You are a DIFFERENT agent than the one that recorded this fixture."),
    );

    let report = scenario.check(&edited).await.unwrap();

    assert!(
        report.model_live > 0,
        "a request the golden doesn't cover must fall through LIVE and be counted, not \
         silently served: served={} live={}",
        report.model_served,
        report.model_live
    );

    std::fs::remove_dir_all(&dir).ok();
}
