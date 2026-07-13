//! Integration test for the checked-in `multi-perspective` example flow (L-37; design in
//! `docs/stories/L-37-multi-perspective-example.md`). Drives the REAL native-text flow
//! (`examples/multi-perspective.flux`) and the REAL checked-in scout role files
//! (`.flux/agents/{tech,product,risk}-scout.md`) through a `FlowClient` with sub-agents wired to a
//! mock provider that returns a canned scout `Answer` JSON per role system prompt — hermetic, no API
//! key.
//!
//! Unlike `strict_review.rs`, this flow's tail calls `synth` (`flux-cognition`'s `CognitionPack`),
//! which is dispatched through the client's TOP-LEVEL provider, not the sub-agent factory. So this
//! test cannot reuse strict_review's panicking `UnusedTopLevelProvider` — one mock type serves BOTH
//! roles (top-level provider for `synth`, sub-agent factory provider for the three scouts),
//! disambiguating on `req.system_text()`. It also mirrors two verified chunk-shape gotchas:
//! `synth`'s `run_model` (`flux-cognition/src/lib.rs`) collects ONLY `Chunk::TextDelta`, while a
//! sub-agent `task` result is read from the adaptive exploration stage's text path. Each scout first
//! emits the required `declare_intent` signal and then returns one `Block(Text)` JSON answer.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use flux_core::{Chunk, ContentBlock, Result, StopReason};
use flux_orchestrate::{RoleRegistry, SubAgents};
use flux_provider::{ChunkStream, Provider, Request};
use flux_runtime::ToolRegistry;
use flux_sdk::FlowClient;
use serde_json::{json, Map, Value};

/// The repo root, resolved from this crate's manifest dir (`crates/flux-sdk` -> repo root).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn read_flow() -> String {
    std::fs::read_to_string(repo_root().join("examples/multi-perspective.flux"))
        .expect("examples/multi-perspective.flux must exist")
}

/// A single canned scout `Answer` JSON, carrying `marker` in its one evidence claim so a later
/// assertion can prove that claim actually reached `synth`'s prompt (branch bind -> `.evidence`
/// field-access -> `merge` composed correctly, not just that the sub-agent ran).
fn scout_answer(summary: &str, marker: &str) -> String {
    json!({
        "status": "answered",
        "summary": summary,
        "evidence": [
            { "claim": { "text": marker, "confidence": 0.9 } }
        ],
        "gaps": [],
        "risks": []
    })
    .to_string()
}

/// A scout `Answer` with NO `evidence` key — the non-conforming case a real LLM can produce. Used to
/// prove the flow's `$technical.evidence?` degrades to empty instead of hard-erroring (L-53).
fn scout_answer_no_evidence(summary: &str) -> String {
    json!({
        "status": "answered",
        "summary": summary,
        "gaps": [],
        "risks": []
    })
    .to_string()
}

/// The canned `synth` answer — the flow's final return value.
fn synth_answer() -> String {
    json!({
        "status": "answered",
        "summary": "Combined technical, product, and risk assessment of the query.",
        "evidence": [
            { "claim": { "text": "synthesized from three scout lenses", "confidence": 0.85 } }
        ],
        "gaps": [],
        "risks": ["needs a human sanity check before shipping"]
    })
    .to_string()
}

/// A provider that serves BOTH roles a hermetic multi-agent flow needs: the client's top-level
/// provider (drives `synth`, a `CognitionPack` op) AND, via the sub-agent factory closure, each
/// scout's provider. Every request's system + first-user-message text is logged (for the
/// "spawned exactly once" / "markers reached synth" assertions) before a canned reply is picked by
/// matching on the system prompt.
struct MultiPerspectiveMockProvider {
    /// One entry per `stream()` call: `"SYSTEM:<system>\nUSER:<first user message text>"`.
    log: Arc<Mutex<Vec<String>>>,
    /// L-53 regression guard: when true, each scout `Answer` OMITS the `evidence` key, so the flow's
    /// `$technical.evidence?` reads a missing field. With the `?` opt-out this must degrade (empty
    /// claim list) rather than hard-erroring the turn after the sub-agents were paid for.
    omit_evidence: bool,
}

#[async_trait]
impl Provider for MultiPerspectiveMockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn stream(&self, req: Request) -> Result<ChunkStream> {
        let intent_stage = req.tools.iter().any(|tool| tool.name == "declare_intent");
        let system = req.system_text().unwrap_or_default();
        let user_text = req
            .messages
            .iter()
            .map(|m| m.text())
            .collect::<Vec<_>>()
            .join("\n");
        self.log.lock().unwrap().push(format!(
            "STAGE:{}\nSYSTEM:{system}\nUSER:{user_text}",
            if intent_stage { "intent" } else { "explore" }
        ));

        if intent_stage {
            let chunks = vec![
                Chunk::Block(ContentBlock::ToolUse {
                    id: "intent".into(),
                    name: "declare_intent".into(),
                    input: json!({
                        "intent": "answer the assigned scout question",
                        "capability_families": [],
                    }),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::ToolUse),
                },
            ];
            return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
        }

        if system.contains("synthesize") {
            // `synth`'s `run_model` collects ONLY `Chunk::TextDelta` (flux-cognition/src/lib.rs)
            // — `Chunk::Block` is never inspected there, so this must be a delta, not a block.
            let chunks = vec![
                Chunk::TextDelta(synth_answer()),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ];
            return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
        }

        let (summary, marker) = if system.contains("TECHNICAL scout") {
            (
                "Architecture supports incremental delivery of partial results.",
                "TECH-MARKER-ALPHA",
            )
        } else if system.contains("PRODUCT scout") {
            (
                "Users need a visible, low-friction way to notice and retry failures.",
                "PROD-MARKER-BETA",
            )
        } else if system.contains("RISK scout") {
            (
                "Partial failures must never silently drop events from the stream.",
                "RISK-MARKER-GAMMA",
            )
        } else {
            panic!("unexpected system prompt (no scout/synth role matched): {system:?}");
        };
        let text = if self.omit_evidence {
            scout_answer_no_evidence(summary)
        } else {
            scout_answer(summary, marker)
        };

        // A sub-agent `task` result is read from the adaptive stage's chat result.
        let chunks = vec![
            Chunk::Block(ContentBlock::Text { text }),
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            },
        ];
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}

fn build_client() -> (FlowClient, Arc<Mutex<Vec<String>>>) {
    build_client_variant(false)
}

fn build_client_variant(omit_evidence: bool) -> (FlowClient, Arc<Mutex<Vec<String>>>) {
    // Load the REAL checked-in scout role files.
    let roles = RoleRegistry::load(&[repo_root().join(".flux/agents")]);
    assert!(
        roles.get("tech-scout").is_some(),
        "tech-scout role must load from .flux/agents"
    );
    assert!(
        roles.get("product-scout").is_some(),
        "product-scout role must load from .flux/agents"
    );
    assert!(
        roles.get("risk-scout").is_some(),
        "risk-scout role must load from .flux/agents"
    );

    let log = Arc::new(Mutex::new(Vec::new()));

    // The scouts answer on their first message (no tool calls needed), so an empty child base
    // registry is enough — mirrors strict_review.rs.
    let child_base = ToolRegistry::new();
    let factory_log = log.clone();
    let factory = Arc::new(move || {
        Ok(Box::new(MultiPerspectiveMockProvider {
            log: factory_log.clone(),
            omit_evidence,
        }) as Box<dyn Provider>)
    });
    let sub_agents = SubAgents::new(roles, child_base, factory, "mock", 4096);

    let top_level = Arc::new(MultiPerspectiveMockProvider {
        log: log.clone(),
        omit_evidence,
    });
    let mut client = FlowClient::builder()
        .model("mock")
        .auto_approve(true)
        .build(top_level, repo_root())
        .expect("build FlowClient");
    client.with_sub_agents(sub_agents);
    (client, log)
}

fn seed_query() -> Map<String, Value> {
    let mut inputs = Map::new();
    inputs.insert(
        "query".to_string(),
        json!("How should flux surface streaming errors?"),
    );
    inputs
}

#[tokio::test]
async fn multi_perspective_fans_out_merges_and_synthesizes_a_cited_answer() {
    let (client, log) = build_client();
    let text = read_flow();

    let out = client
        .run_flow(&text, seed_query())
        .await
        .expect("multi-perspective should execute end-to-end");

    // All three branches bound: exactly 3 `task` calls at the top level (the fixed lens set).
    let task_calls = out.tool_calls.iter().filter(|op| *op == "task").count();
    assert_eq!(
        task_calls, 3,
        "expected exactly 3 scout task calls, got {task_calls} (tool_calls: {:?})",
        out.tool_calls
    );

    let recorded = log.lock().unwrap().clone();

    // Each scout role was spawned exactly once: one intent request and one answer request. Count
    // only the answer stage so protocol staging is explicit without confusing requests with spawns.
    for phrase in ["TECHNICAL scout", "PRODUCT scout", "RISK scout"] {
        let count = recorded
            .iter()
            .filter(|r| r.starts_with("STAGE:explore") && r.contains(phrase))
            .count();
        assert_eq!(
            count, 1,
            "expected `{phrase}` to appear in exactly one recorded request, got {count}: {recorded:?}"
        );
    }

    // The `synth` call's prompt carries all three scouts' evidence — proof that the branch binds,
    // `.evidence` field-access, and `merge` composed the claim lists end-to-end (not just that the
    // sub-agents ran).
    let synth_request = recorded
        .iter()
        .find(|r| r.contains("SYSTEM:") && r.to_lowercase().contains("synthesize"))
        .unwrap_or_else(|| panic!("no synth request recorded: {recorded:?}"));
    for marker in ["TECH-MARKER-ALPHA", "PROD-MARKER-BETA", "RISK-MARKER-GAMMA"] {
        assert!(
            synth_request.contains(marker),
            "synth request must carry {marker} (merged scout evidence), got: {synth_request}"
        );
    }

    // The flow's return value conforms to the prelude `Answer` shape. The declared `-> Answer` is
    // metadata only (not analyzer-enforced) — this IS the enforcement.
    let answer = out.answer().unwrap_or_else(|| {
        panic!(
            "flow result must parse as a prelude Answer, got: {}",
            out.result
        )
    });
    assert_eq!(answer.status, "answered");
    assert!(
        !answer.summary.is_empty(),
        "answer summary must be non-empty"
    );
    assert!(
        !answer.evidence.is_empty(),
        "answer evidence must be non-empty"
    );
}

#[tokio::test]
async fn multi_perspective_is_stable_across_repeated_runs() {
    let (client, _log) = build_client();
    let text = read_flow();

    let out1 = client
        .run_flow(&text, seed_query())
        .await
        .expect("first run should execute end-to-end");
    let out2 = client
        .run_flow(&text, seed_query())
        .await
        .expect("second run should execute end-to-end");

    assert_eq!(
        out1.result, out2.result,
        "multi-perspective must produce identical output for the same inputs across runs"
    );
}

/// L-53 regression guard (code-review finding #1): when a scout omits the `evidence` key — which a
/// real LLM can do — the flow's `$technical.evidence?` optional access must degrade to an empty
/// claim list, NOT hard-error the turn after all three sub-agents were already paid for. Before the
/// `?` migration this bound `$technical.evidence` strictly and aborted with `has no field evidence`.
#[tokio::test]
async fn multi_perspective_degrades_when_a_scout_omits_evidence() {
    let (client, _log) = build_client_variant(/* omit_evidence */ true);
    let text = read_flow();

    let out = client
        .run_flow(&text, seed_query())
        .await
        .expect("flow must complete (degrade) when scouts omit `evidence`, not error");

    // It still fanned out to all three scouts and reached the synthesized answer.
    let task_calls = out.tool_calls.iter().filter(|op| *op == "task").count();
    assert_eq!(
        task_calls, 3,
        "all three lenses still run; got {task_calls}"
    );
    assert!(
        !out.result.trim().is_empty(),
        "the flow returns the synth answer even with no evidence: {:?}",
        out.result
    );
}
