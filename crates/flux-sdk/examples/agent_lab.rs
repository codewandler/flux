//! **The Deterministic Agent Lab** — record your agent once, then test, tune, and resurrect it for
//! $0. The three doors, end to end, in one runnable file.
//!
//! Run with:
//! `cargo run -p codewandler-flux-sdk --features test-kit --example agent_lab`
//!
//! A hermetic mock provider stands in for a real model so the example runs with no API key. In your
//! own project the only difference is that step 1 uses your real provider, once, and the fixture it
//! writes is what you commit.
//!
//! The workflow this demonstrates:
//!
//! 1. **Record** one live turn → `tests/scenarios/<name>/`. Commit it: it's redacted by
//!    construction, and it's an ordinary `Storage::dir` store, so `flux replay|diff --store <dir>`
//!    opens it too.
//! 2. **Test** — `Scenario::load(..).replay(&client)` re-runs the REAL agent offline, under a
//!    deny-all approver and a never-called provider, and `Outcome` asserts on how it *reasoned*
//!    (the canonical Flux-Lang plan) rather than on transcript text. Put this in `cargo test`.
//! 3. **Tune** — `Session::what_if()` re-runs a recorded session under exactly ONE changed
//!    variable against a byte-frozen world, so the diff is a pure causal readout.
//! 4. **Resurrect** — `Session::resurrect()` finishes a turn a crash killed, from the crash point,
//!    with no model re-spend. (Shown as `interrupted()` here: this example never crashes.)

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use flux_core::{Chunk, ContentBlock, Result, StopReason};
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::test::Scenario;
use flux_sdk::tools::{Tool, ToolContext, ToolResult, ToolSpec};
use flux_sdk::{Client, Storage};
use serde_json::json;

/// The agent's one tool: look up an order's refund window. Counts its calls so the example can show
/// that an offline replay never actually runs it.
struct RefundPolicyTool(Arc<AtomicUsize>);

#[async_trait]
impl Tool for RefundPolicyTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "refund_policy",
            "look up the refund window for an order",
            json!({"type": "object", "properties": {"order": {"type": "string"}}}),
        )
    }
    async fn execute(&self, _c: &ToolContext, _p: serde_json::Value) -> Result<ToolResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::ok("30 days"))
    }
}

/// Stands in for a real model: declares an intent, calls the tool, then answers.
struct ScriptedModel {
    round: AtomicUsize,
}

#[async_trait]
impl Provider for ScriptedModel {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, req: Request) -> Result<ChunkStream> {
        let chunks = if req.tools.iter().any(|t| t.name == "declare_intent") {
            vec![
                Chunk::Block(ContentBlock::ToolUse {
                    id: "intent".into(),
                    name: "declare_intent".into(),
                    input: json!({
                        "intent": "answer the refund question",
                        "capability_families": ["core"],
                    }),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::ToolUse),
                },
            ]
        } else if self.round.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                Chunk::Block(ContentBlock::ToolUse {
                    id: "call-0".into(),
                    name: "refund_policy".into(),
                    input: json!({"order": "A-1024"}),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::ToolUse),
                },
            ]
        } else {
            vec![
                Chunk::Block(ContentBlock::Text {
                    text: "Order A-1024 can be refunded within 30 days.".into(),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ]
        };
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}

/// A provider that panics if it is ever called — the proof that step 2 is genuinely offline.
struct NeverProvider;

#[async_trait]
impl Provider for NeverProvider {
    fn name(&self) -> &str {
        "never"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        panic!("an offline replay must never call the model");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("flux-agent-lab-example-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let fixture = dir.join("tests/scenarios/refund-question");
    let live_calls = Arc::new(AtomicUsize::new(0));

    // --- 1. RECORD (once, live) ---------------------------------------------
    // In your project: your real provider, your real key, run once. Then `git add` the fixture.
    let live = Client::builder()
        .model("mock")
        .auto_approve(true)
        .storage(Storage::dir(dir.join("store")))
        .register_op(Arc::new(RefundPolicyTool(live_calls.clone())))
        .build(
            Box::new(ScriptedModel {
                round: AtomicUsize::new(0),
            }),
            &dir,
        )?;
    Scenario::record(&live, "when can order A-1024 be refunded?", &fixture).await?;
    println!("1. recorded → {}", fixture.display());
    println!(
        "   the tool really ran {} time(s)",
        live_calls.load(Ordering::SeqCst)
    );

    // --- 2. TEST (offline, $0, in cargo test) --------------------------------
    // Deny-all approver (the builder default — note: no `auto_approve`) plus a never-called
    // provider. If the replay tried to do anything live, this would fail loudly.
    let replay_calls = Arc::new(AtomicUsize::new(0));
    let offline = Client::builder()
        .model("never")
        .storage(Storage::in_memory())
        .register_op(Arc::new(RefundPolicyTool(replay_calls.clone())))
        .build(Box::new(NeverProvider), &dir)?;

    let outcome = Scenario::load(&fixture)?.replay(&offline).await?;
    outcome.assert_faithful();
    outcome.assert_calls(&["refund_policy"]);
    outcome.assert_text_contains("30 days");
    // The assertion that matters most, and that a transcript can't give you: the agent still
    // *reasons* the same way. `FLUX_GOLDEN=update` re-baselines it after an intended change.
    outcome.assert_plan_snapshot();
    // The safety assertion: this agent must never shell out.
    outcome.assert_never_calls("shell.exec");
    println!(
        "2. replayed offline — the tool ran {} more time(s), the model was never called",
        replay_calls.load(Ordering::SeqCst)
    );
    println!(
        "   plan the assertions ran against:\n{}",
        outcome.plan_source()
    );

    // --- 3. TUNE (one changed variable, byte-frozen world) -------------------
    // "What would my agent have done if the refund policy had said something else?" No model call:
    // the recorded plan re-executes against the frozen world with one output swapped.
    // `Scenario::record` mints its own session on the client's event store, so reach for the most
    // recent one — `session_id()` would lazily mint a fresh, empty default session instead.
    let session = live
        .latest_session()?
        .expect("the recording left a session behind");
    let counterfactual = session
        .what_if()
        .substitute("refund_policy", json!("14 days"))
        .run()
        .await?;
    println!(
        "3. counterfactual · hermetic={} (it never left the recorded world)",
        counterfactual.hermetic()
    );
    match counterfactual.first_divergence() {
        Some(d) => println!("   first divergence at node {}: {}", d.node, d.detail),
        None => println!("   no divergence — the substitution changed nothing"),
    }

    // --- 4. RESURRECT (finish a crashed turn, no model re-spend) -------------
    // Nothing crashed here, so this reports `None`. After a real crash it returns the interrupted
    // turn, and `session.resurrect(&mut sink)` finishes it from exactly where it died — every op
    // with a recorded cassette cell served exactly once, the rest run live through the real
    // envelope. `ClientBuilder::auto_resurrect` (on by default for `Storage::dir`) does it for you
    // on the next turn.
    match session.interrupted()? {
        Some(turn) => println!(
            "4. turn {} was interrupted after {} statement(s) — resurrect() would finish it",
            turn.turn_id, turn.completed
        ),
        None => println!("4. nothing to resurrect — every turn on this session closed cleanly"),
    }

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}
