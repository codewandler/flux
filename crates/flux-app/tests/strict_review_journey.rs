//! L-13 headline acceptance test: the `review_code` journey path and the direct flow path produce the
//! **same** `ReviewReport` for the same inputs (`docs/stories/L-13-strict-review-journey-cli.md`,
//! `docs/designs/strict-review-flows.md` "Journey integration" + "Tests and acceptance").
//!
//! This is a hermetic, no-API-key test: a mock sub-agent [`Provider`] returns canned per-role JSON
//! findings (mirroring `crates/flux-sdk/tests/strict_review.rs`'s fixture). Both drives exercise the
//! SAME `flux_app::review::STRICT_REVIEW_FLOW_SRC` text:
//! - the **journey** path runs `flux_app::App::deliver("review", …)` over
//!   `flux_app::review::strict_review_program()` (a `review_code` journey calling the `strict_review`
//!   composite op, which wraps that text);
//! - the **direct** path runs `flux_sdk::FlowClient::run_flow` on that same text as a bare flow —
//!   exactly the SDK's own `strict_review` integration test.
//!
//! Added RED first (`flux_app::App` had no sub-agent wiring — a journey calling `task` failed with "no
//! sub-agent spawner configured" — and `flux_app::review` did not exist), then made GREEN by wiring
//! `App::with_sub_agents` + the `review` module.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use flux_core::{Chunk, ContentBlock, Result, StopReason};
use flux_orchestrate::{RoleRegistry, SubAgents};
use flux_provider::{ChunkStream, Provider, Request};
use flux_runtime::ToolRegistry;
use serde_json::{json, Map, Value};

/// The repo root, resolved from this crate's manifest dir (`crates/flux-app` -> repo root).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// The same canned-per-role mock fixture `crates/flux-sdk/tests/strict_review.rs` uses, so both the
/// journey and the direct drive see identical reviewer output — the same-report assertion is only
/// meaningful if both paths are handed the same simulated findings.
struct ReviewerMockProvider;

#[async_trait]
impl Provider for ReviewerMockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn stream(&self, req: Request) -> Result<ChunkStream> {
        // `system_text()` joins the segmented system prompt (A-03) — the role prompt now rides in
        // a segment, so `req.system` alone is empty.
        let system = req.system_text().unwrap_or_default();
        let text = if system.contains("SECURITY reviewer") {
            json!([{
                "severity": "critical",
                "category": "security",
                "file": "crates/flux-lang/src/ast.rs",
                "line": 20,
                "title": "security finding",
                "evidence": "seen by the security reviewer",
                "recommendation": "fix it",
                "confidence": 0.95,
                "reviewer": "security"
            }])
            .to_string()
        } else if system.contains("CORRECTNESS reviewer") {
            json!([{
                "severity": "medium",
                "category": "correctness",
                "file": "crates/flux-lang/src/ast.rs",
                "line": 30,
                "title": "correctness finding",
                "evidence": "seen by the correctness reviewer",
                "recommendation": "fix it",
                "confidence": 0.7,
                "reviewer": "correctness"
            }])
            .to_string()
        } else if system.contains("MAINTAINABILITY reviewer") {
            json!([{
                "severity": "low",
                "category": "maintainability",
                "file": "crates/flux-lang/src/ast.rs",
                "line": 40,
                "title": "maintainability finding",
                "evidence": "seen by the maintainability reviewer",
                "recommendation": "fix it",
                "confidence": 0.6,
                "reviewer": "maintainability"
            }])
            .to_string()
        } else {
            panic!("unexpected sub-agent system prompt (no reviewer role matched): {system:?}");
        };

        let chunks = vec![
            Chunk::Block(ContentBlock::Text { text }),
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            },
        ];
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}

/// A never-called top-level provider: neither drive compiles from natural language (the journey runs
/// a stored composite op; the direct path uses `run_flow`'s deterministic `parse` + `execute_with`),
/// so only the sub-agent spawner's `provider_factory` (`ReviewerMockProvider`) is ever invoked.
struct UnusedTopLevelProvider;

#[async_trait]
impl Provider for UnusedTopLevelProvider {
    fn name(&self) -> &str {
        "unused"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        panic!("the top-level provider must never be called by a deterministic run");
    }
}

fn load_review_roles() -> RoleRegistry {
    let roles = RoleRegistry::load(&[repo_root().join(".flux/agents")]);
    assert!(roles.get("review-security").is_some());
    assert!(roles.get("review-correctness").is_some());
    assert!(roles.get("review-maintainability").is_some());
    roles
}

fn sub_agents() -> SubAgents {
    let roles = load_review_roles();
    let child_base = ToolRegistry::new();
    let factory = Arc::new(|| Ok(Box::new(ReviewerMockProvider) as Box<dyn Provider>));
    SubAgents::new(roles, child_base, factory, "mock", 4096)
}

fn seed_files() -> Vec<&'static str> {
    vec!["crates/flux-lang/src/ast.rs"]
}

/// Drive `review_code` as an app journey: `App::deliver("review", {"files": [...]})`.
async fn run_via_journey() -> Value {
    let program = flux_app::review::strict_review_program().expect("build strict-review program");
    let app = flux_app::App::with_sub_agents(
        program,
        None,
        "mock",
        // The checked-in strict-review protocol is a trusted, pre-authored program (the review core
        // itself stays read-only regardless — see the design's security considerations); auto-approve
        // matches how the CLI/SDK's `strict_review` test runs the identical flow (`auto_approve(true)`
        // / `--yes`), so the journey path is compared on equal footing with the direct path.
        true,
        Vec::new(),
        Some(sub_agents()),
    );
    let runs = app
        .deliver("review", json!({ "files": seed_files() }))
        .await
        .expect("review_code journey should run end-to-end");
    assert_eq!(runs.len(), 1, "exactly the `review` trigger's journey ran");
    assert_eq!(runs[0].journey, "review_code");
    serde_json::from_str(&runs[0].result).expect("journey must return a JSON ReviewReport")
}

/// Drive the identical flow text directly through `FlowClient::run_flow` — the SDK's own path
/// (`crates/flux-sdk/tests/strict_review.rs`), using the SAME embedded source
/// (`flux_app::review::STRICT_REVIEW_FLOW_SRC`) and the SAME mock reviewer fixture.
async fn run_via_direct_flow() -> Value {
    let mut client = flux_sdk::FlowClient::builder()
        .model("mock")
        .auto_approve(true)
        .build(Arc::new(UnusedTopLevelProvider), repo_root())
        .expect("build FlowClient");
    client.with_sub_agents(sub_agents());

    let mut inputs = Map::new();
    inputs.insert("files".to_string(), json!(seed_files()));

    let out = client
        .run_flow(flux_app::review::STRICT_REVIEW_FLOW_SRC, inputs)
        .await
        .expect("direct strict_review flow should run end-to-end");
    serde_json::from_str(&out.result).expect("direct flow must return a JSON ReviewReport")
}

#[tokio::test]
async fn journey_and_direct_flow_produce_the_same_review_report() {
    let via_journey = run_via_journey().await;
    let via_direct = run_via_direct_flow().await;

    assert_eq!(
        via_journey, via_direct,
        "the review_code journey and the direct strict_review flow must produce the identical \
         ReviewReport for the same inputs — journey: {via_journey:#}\ndirect: {via_direct:#}"
    );

    // Sanity: this isn't a vacuous comparison of two empty/error reports — both must carry real
    // findings from all three reviewer roles.
    let findings = via_journey["findings"]
        .as_array()
        .expect("report.findings must be an array");
    assert_eq!(findings.len(), 3, "expected one finding per reviewer role");
}
