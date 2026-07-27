//! D-180: the dogfood suite — flux's own agent, pinned by a **committed** golden fixture and re-run
//! hermetically in `cargo test`.
//!
//! `tests/scenarios/coding-agent-note/` is a real recording of flux's adaptive coding agent doing a
//! real task (writing a file into its workspace), checked into the repository: the run's `events.db`
//! plus `flow.db`, its redacted `model.jsonl` cassette, the canonical Flux-Lang plan it produced,
//! and a `scenario.toml` manifest. Nothing in it is machine-specific (no absolute paths, no
//! credentials, no timestamps that affect the assertions), so it reproduces byte-for-byte on any
//! machine.
//!
//! The recording was made against the offline `mock` provider (`flux record --yes -m mock
//! coding-agent-note "write a quick note"`), which is stated plainly rather than dressed up: the
//! agent, the adaptive loop, the plan, the op catalog, and the whole safety envelope are the real
//! ones, and only the model's answers are canned. That is what makes the fixture committable and
//! reproducible in CI at all — a fixture recorded against a live model would be reproducible for
//! replay (that is the point of the cassette) but not re-recordable by anyone without a key.
//!
//! Every test here runs under a deny-all approver and a **never-called provider**: if the replay
//! ever tried to touch the model or dispatch a real op, it would fail loudly instead of silently
//! costing money or writing to disk.
//!
//! Re-baseline with `FLUX_GOLDEN=update cargo test -p codewandler-flux-sdk --features test-kit`.

#![cfg(feature = "test-kit")]

use std::sync::Arc;

use async_trait::async_trait;
use flux_core::Result;
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::test::Scenario;
use flux_sdk::{Client, Storage};

/// The fixture directory, resolved against the crate root so the test runs from anywhere.
fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scenarios/coding-agent-note")
}

/// Panics if the model is ever called. A hermetic replay must never reach it.
struct NeverProvider;

#[async_trait]
impl Provider for NeverProvider {
    fn name(&self) -> &str {
        "never"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        panic!("the golden replay must never call the model");
    }
}

/// Answers any request with prose, counting the calls — the "a request the golden doesn't cover
/// falls through LIVE" probe. Declares intent first, as the engine's adaptive loop requires.
struct CountingProseProvider(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait]
impl Provider for CountingProseProvider {
    fn name(&self) -> &str {
        "counting"
    }
    async fn stream(&self, req: Request) -> Result<ChunkStream> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let chunks = if req.tools.iter().any(|t| t.name == "declare_intent") {
            vec![
                flux_core::Chunk::Block(flux_core::ContentBlock::ToolUse {
                    id: "intent".into(),
                    name: "declare_intent".into(),
                    input: serde_json::json!({
                        "intent": "answer directly",
                        "capability_families": ["core"],
                    }),
                }),
                flux_core::Chunk::Done {
                    stop_reason: Some(flux_core::StopReason::ToolUse),
                },
            ]
        } else {
            vec![
                flux_core::Chunk::Block(flux_core::ContentBlock::Text {
                    text: "a completely different answer".into(),
                }),
                flux_core::Chunk::Done {
                    stop_reason: Some(flux_core::StopReason::EndTurn),
                },
            ]
        };
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}

/// The offline client the golden replays against: builder default approver (deny-all — no
/// `auto_approve`), a never-called provider, and ephemeral storage.
fn offline_client(tag: &str) -> Client {
    let dir = std::env::temp_dir().join(format!("flux-golden-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    Client::builder()
        .model("never")
        .storage(Storage::in_memory())
        .build(Box::new(NeverProvider), &dir)
        .unwrap()
}

/// The dogfood headline: flux's own agent re-runs its committed golden hermetically, reproduces the
/// exact canonical plan it planned when the recording was made, and costs nothing.
#[tokio::test]
async fn the_flux_coding_agent_reproduces_its_committed_golden_for_free() {
    let scenario = Scenario::load(fixture()).expect("load the committed golden fixture");
    assert_eq!(scenario.manifest().input, "write a quick note");

    let outcome = scenario
        .replay(&offline_client("headline"))
        .await
        .expect("replay the golden");

    // The world: no divergence, and every recorded cassette cell was consumed.
    outcome.assert_faithful();
    // The reasoning: the canonical Flux-Lang plan is byte-identical to the committed snapshot.
    outcome.assert_plan_snapshot();
    // The agent really did the task — it wrote a file, it didn't just talk about it.
    outcome.assert_calls(&["append"]);
    // The safety assertion an embedder actually cares about: this agent never shells out.
    outcome.assert_never_calls("shell.exec");
    outcome.assert_never_calls("process.exec");
    // And it is free: a replay makes no model call, so there is nothing to price.
    outcome.assert_cost_under(0.000_001);
}

/// The fixture is committed-safe: it carries no absolute paths from the machine that recorded it,
/// and no unredacted secret shape. This is the property that makes checking a recording into a
/// repository defensible at all, so it is pinned rather than assumed.
#[test]
fn the_committed_fixture_is_portable_and_redacted() {
    let dir = fixture();
    for name in ["model.jsonl", "plan.flux.snap", "scenario.toml"] {
        let text = std::fs::read_to_string(dir.join(name)).expect(name);
        assert!(
            !text.contains("/home/") && !text.contains("/Users/"),
            "{name} carries an absolute path from the recording machine"
        );
    }
    let manifest = std::fs::read_to_string(dir.join("scenario.toml")).unwrap();
    assert!(
        manifest.contains("redaction_marker"),
        "the manifest must pin the redactor's marker so the dual-hash matcher can't drift"
    );
    for name in ["events.db", "flow.db"] {
        assert!(dir.join(name).exists(), "{name} is part of the fixture");
    }
}

/// D-180's A/B demonstration, half one — a **reasoning** regression. Changing the system prompt
/// makes the agent plan something different, and `Scenario::check` classifies that as
/// `DiffRow::Plan`: the plan changed, not the world.
///
/// The model cassette is keyed on a canonicalized request hash, so an edited system prompt no longer
/// matches any recorded call — which is exactly why `check` counts the fall-through
/// (`Report::model_live`) instead of quietly serving a stale answer.
#[tokio::test]
async fn editing_the_system_prompt_surfaces_a_plan_divergence() {
    let scenario = Scenario::load(fixture()).expect("load the golden");
    let dir = std::env::temp_dir().join(format!("flux-golden-prompt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // NOT a never-provider here: proving that an uncovered request falls through LIVE requires a
    // provider that can actually answer one. It counts its calls so the test can pin that the
    // fall-through really happened, and answers in prose so the re-drive completes.
    let live_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let edited = Client::builder()
        .model("never")
        .system_prompt("You are a COMPLETELY different agent than the one that recorded this.")
        .storage(Storage::in_memory())
        .build(Box::new(CountingProseProvider(live_calls.clone())), &dir)
        .unwrap();

    let report = scenario.check(&edited).await.expect("check the golden");

    assert!(
        live_calls.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "the edited prompt really did reach the live provider"
    );
    assert!(
        report.model_live > 0,
        "an edited prompt is a request the golden cassette doesn't cover — it must be COUNTED as a \
         live fall-through, never silently served: served={} live={}",
        report.model_served,
        report.model_live
    );
    assert!(
        !report.is_clean(),
        "a prompt edit that changes the agent's reasoning is not a clean re-drive: {:?}",
        report.render()
    );
}

/// D-180's A/B demonstration, half two — a **world** regression. Substituting a different output for
/// an op the recording ran leaves the plan untouched and surfaces a `DiffRow::Output`: same
/// reasoning, different world. This is the distinction that makes the Lab useful — "my agent thinks
/// differently now" and "my agent's tools answered differently" are different bugs.
#[tokio::test]
async fn substituting_a_tool_output_surfaces_a_world_divergence_with_the_plan_intact() {
    use flux_events::DiffRow;

    // Replay the golden into a real session first, so there is something to run a what-if over.
    let scenario = Scenario::load(fixture()).expect("load the golden");
    let client = offline_client("world");
    let outcome = scenario.replay(&client).await.expect("replay the golden");
    outcome.assert_faithful();

    // The replayed session lives in the scenario's own work-dir store, so open it from there.
    let events = Arc::new(
        flux_events::EventStore::open(scenario.work_dir().join("events.db")).expect("work store"),
    );
    let session_id = events
        .latest_session()
        .expect("latest")
        .expect("the replay recorded a session");
    let replayed = Client::builder()
        .model("never")
        .storage(Storage::custom(
            events.clone(),
            flux_flow::state::FlowStore::in_memory_with_events(events.clone()).unwrap(),
        ))
        .build(Box::new(NeverProvider), std::env::temp_dir())
        .unwrap()
        .open_session(&session_id)
        .expect("open the replayed session");

    let cf = replayed
        .what_if()
        .substitute(
            "append",
            serde_json::json!("a DIFFERENT answer from the world"),
        )
        .run()
        .await
        .expect("run the counterfactual");

    // A pure substitution never leaves the pinned world and never calls the model.
    assert!(cf.hermetic(), "a pure substitution stays on the tape");

    let diff = cf.diff().expect("diff");
    assert!(!diff.identical, "the substituted output must show up");
    let plan_rows = diff
        .rows
        .iter()
        .filter(|r| matches!(r, DiffRow::Plan { .. }))
        .count();
    let output_rows: Vec<_> = diff
        .rows
        .iter()
        .filter(|r| matches!(r, DiffRow::Output { .. }))
        .collect();
    assert_eq!(
        plan_rows, 0,
        "the agent reasoned identically — only the world changed: {diff:?}"
    );
    assert_eq!(output_rows.len(), 1, "exactly the substituted op: {diff:?}");
    match output_rows[0] {
        DiffRow::Output { op, b, .. } => {
            assert_eq!(op, "append");
            assert_eq!(b, "a DIFFERENT answer from the world");
        }
        _ => unreachable!(),
    }
}
