//! D-195: `Scenario`'s LLM-judge assertion — the complementary axis to `replay`'s exact/
//! deterministic plan assertions, for TEXT outputs that don't have one canonical answer.
//!
//! Only compiled/run under `--features test-kit` (mirrors `tests/agent_test_kit.rs`'s convention).

#![cfg(feature = "test-kit")]
#![allow(clippy::await_holding_lock)]

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use flux_core::{ContentBlock, Result, StopReason};
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::test::{Rubric, Scenario};
use flux_sdk::tools::{Tool, ToolContext, ToolResult, ToolSpec};
use flux_sdk::{Client, Storage};
use serde_json::json;

// --- shared test plumbing (mirrors tests/agent_test_kit.rs) -----------------

/// Serializes every test in this file — `Scenario::judge`'s `FLUX_GOLDEN=update` branch reads a
/// process-wide env var, exactly like `Scenario::record`/`check` (see `agent_test_kit.rs`'s own
/// `env_lock` doc for the full rationale).
fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn serialize() -> std::sync::MutexGuard<'static, ()> {
    env_lock().lock().unwrap_or_else(|e| e.into_inner())
}

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
        "flux-sdk-judge-test-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
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

fn request_has_tool(req: &Request, name: &str) -> bool {
    req.tools.iter().any(|t| t.name == name)
}

/// A no-op op the recorded agent turn calls once, so the fixture has something to answer about.
struct NoopTool;
#[async_trait]
impl Tool for NoopTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "bump",
            "bump a counter",
            json!({"type": "object", "properties": {}}),
        )
    }
    async fn execute(&self, _c: &ToolContext, _params: serde_json::Value) -> Result<ToolResult> {
        Ok(ToolResult::ok("count=1"))
    }
}

/// The agent half of the mock: declares intent, calls `bump` once, then answers in prose. Never
/// invoked by a judge call — see `is_judge_request`.
struct ScriptedAgent {
    answer: &'static str,
    calls: AtomicUsize,
}
#[async_trait]
impl Provider for ScriptedAgent {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, req: Request) -> Result<ChunkStream> {
        if request_has_tool(&req, "declare_intent") {
            return Ok(chunk_stream(intent_chunks(
                "answer the question",
                &["core"],
            )));
        }
        let n = self.calls.fetch_add(1, Ordering::Relaxed);
        if n == 0 {
            return Ok(chunk_stream(vec![
                flux_core::Chunk::Block(ContentBlock::ToolUse {
                    id: "call-0".into(),
                    name: "bump".into(),
                    input: json!({}),
                }),
                flux_core::Chunk::Done {
                    stop_reason: Some(StopReason::ToolUse),
                },
            ]));
        }
        Ok(chunk_stream(prose_chunks(self.answer)))
    }
}

/// A request is a judge call (never the agent's own turn) iff its first message is the fixed
/// `"Criterion:\n..."` shape `Scenario::judge` builds — the only thing distinguishing the two
/// roles sharing one `Client`/`Provider` in these tests.
fn is_judge_request(req: &Request) -> bool {
    req.messages
        .first()
        .map(|m| m.text().starts_with("Criterion:"))
        .unwrap_or(false)
}

/// Wraps [`ScriptedAgent`] and additionally answers judge calls with a canned verdict, counting
/// how many judge calls actually reached it live — the "record run spends, replay run doesn't"
/// proof needs an honest counter, not just an absence-of-panic.
struct JudgeCapableMock {
    agent: ScriptedAgent,
    verdict_json: &'static str,
    live_judge_calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Provider for JudgeCapableMock {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, req: Request) -> Result<ChunkStream> {
        if is_judge_request(&req) {
            self.live_judge_calls.fetch_add(1, Ordering::SeqCst);
            return Ok(chunk_stream(prose_chunks(self.verdict_json)));
        }
        self.agent.stream(req).await
    }
}

/// A never-called provider — panics if the model (agent OR judge) is ever hit. Pairs with a
/// deny-all approver to prove a plain (non-`FLUX_GOLDEN=update`) `Scenario::judge` call never
/// touches the model, hit or miss.
struct NeverProvider;
#[async_trait]
impl Provider for NeverProvider {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        panic!("a plain (non-FLUX_GOLDEN=update) judge call must never invoke the model");
    }
}

fn record_client(dir: &std::path::Path, answer: &'static str) -> Client {
    Client::builder()
        .model("mock")
        .auto_approve(true)
        .storage(Storage::in_memory())
        .register_op(Arc::new(NoopTool))
        .build(
            Box::new(ScriptedAgent {
                answer,
                calls: AtomicUsize::new(0),
            }),
            dir,
        )
        .unwrap()
}

fn judge_capable_client(
    dir: &std::path::Path,
    answer: &'static str,
    verdict_json: &'static str,
    live_judge_calls: Arc<AtomicUsize>,
) -> Client {
    Client::builder()
        .model("mock")
        .auto_approve(true)
        .storage(Storage::in_memory())
        .register_op(Arc::new(NoopTool))
        .build(
            Box::new(JudgeCapableMock {
                agent: ScriptedAgent {
                    answer,
                    calls: AtomicUsize::new(0),
                },
                verdict_json,
                live_judge_calls,
            }),
            dir,
        )
        .unwrap()
}

fn offline_client(dir: &std::path::Path) -> Client {
    Client::builder()
        .model("mock")
        .storage(Storage::in_memory())
        .register_op(Arc::new(NoopTool))
        .build(Box::new(NeverProvider), dir)
        .unwrap()
}

const PASS_VERDICT: &str = r#"{"passed": true, "rationale": "the text clearly says count=1"}"#;
const FAIL_VERDICT: &str =
    r#"{"passed": false, "rationale": "the text never mentions the refund policy"}"#;

// --- acceptance tests --------------------------------------------------------

/// Acceptance #1/#2: a fresh fixture has no judge verdict recorded yet — `judge()` must refuse to
/// spend silently. This is the failing-first test: at the time this file was written,
/// `flux_sdk::test::Rubric` and `Scenario::judge`/`assert_judge` did not exist at all, so this
/// whole file failed to COMPILE (the right kind of "fails first" for a brand-new API surface).
#[tokio::test]
async fn judge_refuses_to_spend_on_a_cache_miss_without_flux_golden_update() {
    let _serialize = serialize();
    let dir = tmp_dir("miss-no-update");
    let fixture = dir.join("scenario-fixture");
    let client = record_client(&dir, "bumped it (count=1)");
    let scenario = Scenario::record(&client, "bump once and report the count", &fixture)
        .await
        .unwrap();

    // No FLUX_GOLDEN=update, no prior judge.jsonl entry: this must be a hard error, and the
    // NeverProvider proves it never fell through to a live call either.
    let never = offline_client(&dir);
    let rubric = Rubric::model("mock");
    let err = scenario
        .judge(
            &never,
            "the answer reports a count",
            "bumped it (count=1)",
            &rubric,
        )
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("FLUX_GOLDEN=update"),
        "expected an actionable re-record hint, got: {msg}"
    );
}

/// Acceptance #1/#2/#4: `FLUX_GOLDEN=update` makes the judge call live (spends exactly once,
/// against the explicit `rubric.model`) and commits the verdict; a later plain call against a
/// NEVER-called provider is served entirely from that committed cassette — proving the replay
/// path constructs no live call at all (mirrors how `replay`'s hermeticity is proven elsewhere in
/// this crate: pair with a provider that panics if invoked, and show it never panics).
#[tokio::test]
async fn judge_call_flows_through_the_cassette_record_once_replay_free() {
    let _serialize = serialize();
    let dir = tmp_dir("record-then-replay");
    let fixture = dir.join("scenario-fixture");
    let live_judge_calls = Arc::new(AtomicUsize::new(0));
    let client = judge_capable_client(
        &dir,
        "bumped it (count=1)",
        PASS_VERDICT,
        live_judge_calls.clone(),
    );
    let scenario = Scenario::record(&client, "bump once and report the count", &fixture)
        .await
        .unwrap();

    let rubric = Rubric::model("mock");
    {
        let _update = EnvGuard::set("FLUX_GOLDEN", "update");
        let verdict = scenario
            .judge(
                &client,
                "the answer reports a count",
                "bumped it (count=1)",
                &rubric,
            )
            .await
            .unwrap();
        assert!(verdict.passed, "rationale: {}", verdict.rationale);
        assert_eq!(
            live_judge_calls.load(Ordering::SeqCst),
            1,
            "exactly one live judge call for the recording run"
        );
    }
    assert!(
        fixture.join("judge.jsonl").exists(),
        "the verdict must be committed to the fixture's own judge cassette"
    );

    // Explicit model, no hidden default: the committed record names exactly the rubric's model.
    let committed = std::fs::read_to_string(fixture.join("judge.jsonl")).unwrap();
    let line: serde_json::Value = serde_json::from_str(committed.lines().next().unwrap()).unwrap();
    assert_eq!(line["model"], "mock");

    // Now replay with a provider that panics if the model is EVER invoked (agent or judge) — a
    // hit must serve straight from `judge.jsonl` and cost nothing.
    let never = offline_client(&dir);
    let verdict = scenario
        .judge(
            &never,
            "the answer reports a count",
            "bumped it (count=1)",
            &rubric,
        )
        .await
        .unwrap();
    assert!(verdict.passed, "rationale: {}", verdict.rationale);
    assert_eq!(
        live_judge_calls.load(Ordering::SeqCst),
        1,
        "the replay must not have made a second live judge call"
    );
}

/// Acceptance #3: a changed judged text must never silently reuse a stale committed verdict — its
/// canonical request hashes differently, so it is a cache MISS, which (absent
/// `FLUX_GOLDEN=update`) is a loud error rather than a quiet pass.
#[tokio::test]
async fn changed_target_invalidates_the_recorded_verdict_loudly() {
    let _serialize = serialize();
    let dir = tmp_dir("stale-target");
    let fixture = dir.join("scenario-fixture");
    let live_judge_calls = Arc::new(AtomicUsize::new(0));
    let client = judge_capable_client(
        &dir,
        "bumped it (count=1)",
        PASS_VERDICT,
        live_judge_calls.clone(),
    );
    let scenario = Scenario::record(&client, "bump once and report the count", &fixture)
        .await
        .unwrap();
    let rubric = Rubric::model("mock");
    {
        let _update = EnvGuard::set("FLUX_GOLDEN", "update");
        scenario
            .judge(
                &client,
                "the answer reports a count",
                "bumped it (count=1)",
                &rubric,
            )
            .await
            .unwrap();
    }

    // Same criterion/rubric, a DIFFERENT (regressed) target: must miss the cassette and refuse to
    // silently pass — proven by both the `Err` and the never-called provider staying uninvoked.
    let never = offline_client(&dir);
    let err = scenario
        .judge(
            &never,
            "the answer reports a count",
            "a completely different, regressed answer",
            &rubric,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("FLUX_GOLDEN=update"));
}

/// Acceptance #1: a FAILING verdict's rationale is surfaced — a red assertion says *why*, not
/// just that it failed. `assert_judge`'s panic path is exactly `Verdict::assert_pass`, exercised
/// directly here (a synchronous panic, so no `catch_unwind`-around-an-`.await` plumbing needed);
/// `judge_call_flows_through_the_cassette_record_once_replay_free` above already proves
/// `assert_judge` reaches a real verdict end-to-end.
#[tokio::test]
async fn a_failing_verdict_panics_with_the_judges_rationale() {
    let _serialize = serialize();
    let dir = tmp_dir("fail-verdict");
    let fixture = dir.join("scenario-fixture");
    let live_judge_calls = Arc::new(AtomicUsize::new(0));
    let client = judge_capable_client(&dir, "bumped it (count=1)", FAIL_VERDICT, live_judge_calls);
    let scenario = Scenario::record(&client, "bump once and report the count", &fixture)
        .await
        .unwrap();
    let rubric = Rubric::model("mock");

    let verdict = {
        let _update = EnvGuard::set("FLUX_GOLDEN", "update");
        scenario
            .judge(
                &client,
                "the answer cites the refund policy",
                "bumped it (count=1)",
                &rubric,
            )
            .await
            .unwrap()
    };
    assert!(!verdict.passed);
    assert!(verdict.rationale.contains("refund policy"));

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| verdict.assert_pass()));
    let err = result.expect_err("assert_pass must panic on a failing verdict");
    let msg = err.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(
        msg.contains("refund policy"),
        "expected the judge's rationale in the panic message, got: {msg}"
    );
}
