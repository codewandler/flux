//! Integration test for the checked-in `strict_review` example flow (L-10 Phase 1 + L-12 Phase 3;
//! design `docs/designs/strict-review-flows.md`). Drives the REAL native-text flow
//! (`examples/strict_review.flux`) and the REAL checked-in reviewer role files
//! (`.flux/agents/review-*.md`) through a `FlowClient` with sub-agents wired to a mock provider that
//! returns different canned JSON findings per reviewer role — hermetic, no API key.
//!
//! Added RED first (the flow + role files didn't exist), then made GREEN by L-10's implementation.
//! L-12 migrated the aggregation from inline `filter`/`dedupe`/`sort` over a model-emitted `rank` to
//! the native `review.aggregate` op: reviewers no longer emit `fingerprint`/`rank` (the aggregator
//! computes both), a malformed entry is quarantined into `gaps` instead of silently dropped, and
//! ranking is severity -> confidence -> agreement.

use std::path::PathBuf;
use std::sync::Arc;

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
    std::fs::read_to_string(repo_root().join("examples/strict_review.flux"))
        .expect("examples/strict_review.flux must exist")
}

/// A provider that returns different canned reviewer JSON depending on which role's system prompt it
/// is asked to drive. Every reply is a single JSON array of findings — the exact shape a reviewer role
/// is instructed to emit (no `fingerprint`/`rank` — the aggregator computes both now).
///
/// Findings are rigged to exercise the required aggregation behaviors:
/// - "shared finding" is emitted by BOTH the security and correctness reviewer with the same
///   category/file/line/title (so it fingerprints identically) — must collapse to one finding with
///   `agreement == 2` after `review.aggregate`.
/// - the maintainability reviewer's array contains one MALFORMED entry (a bare string, not an object)
///   alongside a well-formed finding — it must land in `gaps`, not crash the pipeline and not be
///   surfaced as a finding.
struct ReviewerMockProvider;

#[async_trait]
impl Provider for ReviewerMockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn stream(&self, req: Request) -> Result<ChunkStream> {
        if req.tools.iter().any(|tool| tool.name == "declare_intent") {
            let chunks = vec![
                Chunk::Block(ContentBlock::ToolUse {
                    id: "intent".into(),
                    name: "declare_intent".into(),
                    input: json!({
                        "intent": "review the assigned files",
                        "capability_families": [],
                    }),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::ToolUse),
                },
            ];
            return Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))));
        }
        let system = req.system_text().unwrap_or_default();
        let text = if system.contains("SECURITY reviewer") {
            json!([
                {
                    "severity": "high",
                    "category": "shared-cat",
                    "file": "crates/flux-lang/src/ast.rs",
                    "line": 10,
                    "title": "shared finding",
                    "evidence": "seen by two reviewers",
                    "recommendation": "fix it",
                    "confidence": 0.9,
                    "reviewer": "security"
                },
                {
                    "severity": "critical",
                    "category": "security",
                    "file": "crates/flux-lang/src/ast.rs",
                    "line": 20,
                    "title": "security-only finding",
                    "evidence": "only the security reviewer sees this",
                    "recommendation": "fix it",
                    "confidence": 0.95,
                    "reviewer": "security"
                }
            ])
            .to_string()
        } else if system.contains("CORRECTNESS reviewer") {
            json!([
                {
                    "severity": "high",
                    "category": "shared-cat",
                    "file": "crates/flux-lang/src/ast.rs",
                    "line": 10,
                    "title": "shared finding",
                    "evidence": "seen by two reviewers",
                    "recommendation": "fix it",
                    "confidence": 0.85,
                    "reviewer": "correctness"
                },
                {
                    "severity": "medium",
                    "category": "correctness",
                    "file": "crates/flux-lang/src/ast.rs",
                    "line": 30,
                    "title": "correctness-only finding",
                    "evidence": "only the correctness reviewer sees this",
                    "recommendation": "fix it",
                    "confidence": 0.7,
                    "reviewer": "correctness"
                }
            ])
            .to_string()
        } else if system.contains("MAINTAINABILITY reviewer") {
            json!([
                "this entry is malformed — a bare string, not a finding object",
                {
                    "severity": "low",
                    "category": "maintainability",
                    "file": "crates/flux-lang/src/ast.rs",
                    "line": 40,
                    "title": "maintainability-only finding",
                    "evidence": "only the maintainability reviewer sees this",
                    "recommendation": "fix it",
                    "confidence": 0.6,
                    "reviewer": "maintainability"
                }
            ])
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

/// A never-called top-level provider: `run_flow` (`parse` + `analyze` + `execute_with`) never
/// is fully authored, so the client's own provider is never invoked — only the
/// sub-agent spawner's `provider_factory` (`ReviewerMockProvider`) is.
struct UnusedTopLevelProvider;

#[async_trait]
impl Provider for UnusedTopLevelProvider {
    fn name(&self) -> &str {
        "unused"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        panic!("the top-level provider must never be called by a deterministic run_flow");
    }
}

fn build_client() -> FlowClient {
    // Load the REAL checked-in reviewer role files, exercising the actual restricted role
    // definitions (frontmatter `tools: []`) the story ships.
    let roles = RoleRegistry::load(&[repo_root().join(".flux/agents")]);
    assert!(
        roles.get("review-security").is_some(),
        "review-security role must load from .flux/agents"
    );
    assert!(
        roles.get("review-correctness").is_some(),
        "review-correctness role must load from .flux/agents"
    );
    assert!(
        roles.get("review-maintainability").is_some(),
        "review-maintainability role must load from .flux/agents"
    );
    // Each restricted role declares `tools: []`; the child_base being empty too makes the
    // "no filesystem/shell tools" restriction doubly explicit at the harness level.
    let child_base = ToolRegistry::new();
    let factory = Arc::new(|| Ok(Box::new(ReviewerMockProvider) as Box<dyn Provider>));
    let sub_agents = SubAgents::new(roles, child_base, factory, "mock", 4096);

    let mut client = FlowClient::builder()
        .model("mock")
        .auto_approve(true)
        .build(Arc::new(UnusedTopLevelProvider), repo_root())
        .expect("build FlowClient");
    client.with_sub_agents(sub_agents);
    client
}

fn seed_files() -> Map<String, Value> {
    let mut inputs = Map::new();
    inputs.insert("files".to_string(), json!(["crates/flux-lang/src/ast.rs"]));
    inputs
}

#[tokio::test]
async fn strict_review_produces_a_typed_report_with_bounded_deterministic_aggregation() {
    let client = build_client();
    let text = read_flow();

    let out = client
        .run_flow(&text, seed_files())
        .await
        .expect("strict_review should execute end-to-end");

    // Fan-out is bounded: exactly 3 `task` calls (the declared reviewer set), never model-chosen.
    let task_calls = out.tool_calls.iter().filter(|op| *op == "task").count();
    assert_eq!(
        task_calls, 3,
        "expected exactly 3 bounded reviewer task calls, got {task_calls} (tool_calls: {:?})",
        out.tool_calls
    );

    let report: Value = serde_json::from_str(&out.result)
        .expect("strict_review must return a JSON ReviewReport object");

    // Structured report shape.
    assert!(report.get("summary").is_some(), "report missing `summary`");
    assert!(
        report.get("checked_files").is_some(),
        "report missing `checked_files`"
    );
    let reviewers = report["reviewers"]
        .as_array()
        .expect("report.reviewers must be an array");
    assert_eq!(
        reviewers.len(),
        3,
        "report must name exactly 3 reviewers, got {reviewers:?}"
    );

    let findings = report["findings"]
        .as_array()
        .expect("report.findings must be an array");

    // Findings from EACH reviewer role are present.
    let reviewer_names: std::collections::BTreeSet<&str> = findings
        .iter()
        .filter_map(|f| f.get("reviewer").and_then(|r| r.as_str()))
        .collect();
    assert!(
        reviewer_names.contains("security"),
        "missing security reviewer findings: {findings:?}"
    );
    assert!(
        reviewer_names.contains("correctness"),
        "missing correctness reviewer findings: {findings:?}"
    );
    assert!(
        reviewer_names.contains("maintainability"),
        "missing maintainability reviewer findings: {findings:?}"
    );

    // Deduplication: "shared finding" was emitted by BOTH the security and correctness reviewer with
    // the same category/file/line/title — it must collapse to exactly one finding, with agreement
    // counting the 2 distinct reviewers that raised it (the aggregator computes the fingerprint; the
    // mock reviewers never emit one).
    let shared: Vec<&Value> = findings
        .iter()
        .filter(|f| f.get("title").and_then(|v| v.as_str()) == Some("shared finding"))
        .collect();
    assert_eq!(
        shared.len(),
        1,
        "the shared finding must dedupe to exactly one entry, found {}: {findings:?}",
        shared.len()
    );
    assert_eq!(
        shared[0]["agreement"], 2,
        "the shared finding must record agreement == 2 (security + correctness): {shared:?}"
    );
    // Every other finding was raised by exactly one reviewer.
    for f in findings {
        if f["title"] != "shared finding" {
            assert_eq!(f["agreement"], 1, "unexpected agreement for {f:?}");
        }
    }

    // The malformed bare-string entry from the maintainability reviewer must be quarantined into
    // `gaps` — never surfaced verbatim as a finding, never silently dropped.
    assert!(
        !findings
            .iter()
            .any(|f| f.as_str()
                == Some("this entry is malformed — a bare string, not a finding object")),
        "the malformed entry must not be surfaced verbatim as a finding: {findings:?}"
    );
    let gaps = report["gaps"]
        .as_array()
        .expect("report.gaps must be an array");
    assert_eq!(
        gaps.len(),
        1,
        "the one malformed maintainability entry must produce exactly one gap: {gaps:?}"
    );
    assert!(
        gaps[0]
            .as_str()
            .unwrap()
            .contains("this entry is malformed — a bare string, not a finding object"),
        "the gap must describe the dropped malformed entry: {gaps:?}"
    );

    // Expect exactly the well-formed findings: shared (collapsed), sec-only, corr-only, maint-only.
    assert_eq!(
        findings.len(),
        4,
        "expected 4 well-formed findings after dedupe (shared collapsed once + 3 unique), got {}: {findings:?}",
        findings.len()
    );

    // Ranked severity -> confidence -> agreement: security-only(critical) first, then the shared
    // finding and correctness-only both at distinct severities (high, medium), then
    // maintainability-only(low) last. The whole list must be non-increasing by severity rank.
    fn severity_rank(sev: &str) -> u8 {
        match sev {
            "critical" => 4,
            "high" => 3,
            "medium" => 2,
            "low" => 1,
            _ => 0,
        }
    }
    let titles: Vec<&str> = findings
        .iter()
        .map(|f| f["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        vec![
            "security-only finding",        // critical
            "shared finding",               // high
            "correctness-only finding",     // medium
            "maintainability-only finding"  // low
        ],
        "unexpected ranking order: {titles:?}"
    );
    let ranks: Vec<u8> = findings
        .iter()
        .map(|f| severity_rank(f["severity"].as_str().unwrap()))
        .collect();
    let mut sorted_desc = ranks.clone();
    sorted_desc.sort_by(|a, b| b.cmp(a));
    assert_eq!(
        ranks, sorted_desc,
        "findings must be ranked in non-increasing severity order, got {ranks:?}"
    );
}

#[tokio::test]
async fn strict_review_is_stable_across_repeated_runs() {
    let client = build_client();
    let text = read_flow();

    let out1 = client
        .run_flow(&text, seed_files())
        .await
        .expect("first run should execute end-to-end");
    let out2 = client
        .run_flow(&text, seed_files())
        .await
        .expect("second run should execute end-to-end");

    // Same inputs -> identical ordering across two independent runs (deterministic aggregation).
    assert_eq!(
        out1.result, out2.result,
        "strict_review must produce identical ordering for the same inputs across runs"
    );
}
