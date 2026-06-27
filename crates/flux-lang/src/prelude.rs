//! The artifact-type **prelude** — the curated standard library of typed structures an agent task
//! manipulates (claims, evidence, needs, context packs, patches, …). These are *not* new `Value`
//! variants: every artifact is an ordinary [`crate::ast::Value::Struct`] whose `Named` [`crate::ast::TypeRef`]
//! names one of these schemas. The prelude ships with the language so ops can declare their inputs and
//! outputs in these terms; an op's `Named("Claim")` input lowers to a `#/$defs/Claim` `$ref`
//! (`opspec::type_ref_to_schema`) that resolves against [`prelude_schema`].
//!
//! The types double as the Rust builders/readers the SDK exposes (a `Claim` is a real struct, not a
//! loose map). They are **new and distinct** from `flux_evidence::Observation` (a generic audit bag,
//! `{ kind, phase, data }`): the only link is one-directional — a produced `Evidence` *may* be recorded
//! into an `EvidenceLog` as an `Observation`.
//!
//! This module is the SSOT for the ontology the same way [`crate::schema`] is for node kinds:
//! [`prelude_type_catalog`] generates the markdown table from the struct doc-comments, and a drift test
//! (`tests/skill_in_sync.rs`) keeps `docs/reference.md` in step.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ast::ThingRef;

/// A cited region inside a source document — the proof pointer a `Claim` or `Evidence` points at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Span {
    /// The source the region lives in (a file, ticket, URL, …).
    pub source: ThingRef,
    /// The region within the source — a line range, char offset, or selector string.
    pub range: String,
}

/// A factual assertion extracted from a source, carrying its provenance span and a confidence score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Claim {
    /// The assertion text.
    pub text: String,
    /// The cited span backing the claim, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<Span>,
    /// Confidence in the claim, in `[0, 1]`.
    #[serde(default)]
    pub confidence: f64,
}

/// A claim together with the supporting spans that ground it — the audited unit of support.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    /// The claim being supported.
    pub claim: Claim,
    /// The spans that back the claim.
    #[serde(default)]
    pub support: Vec<Span>,
}

/// An explicit statement of missing information: what to ask, which fields are required to satisfy it,
/// and the condition under which it is considered met. Produced by the pure `need` op; its complement
/// `gaps` reports the still-unmet `require` fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Need {
    /// The open question to resolve.
    pub ask: String,
    /// The fields that must be filled for the need to be met.
    #[serde(default)]
    pub require: Vec<String>,
    /// A predicate the explicit loop checks to decide the need is satisfied (a plain field — the
    /// interpreter does not evaluate it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_when: Option<String>,
}

/// A bounded, intentionally-budgeted bundle of context — the value produced by the `ctx`/`ctx_append`
/// nodes. `members` are the symbol references selected into the pack; `budget` is the char/token cap the
/// runtime shrinks the pack to at node evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Ctx {
    /// The pack's name (the symbol it binds to).
    pub name: String,
    /// Why the pack exists — seeds the audit trail and any model prompt that consumes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// The symbol references included in the pack, after budgeting.
    #[serde(default)]
    pub members: Vec<String>,
    /// The char/token budget the pack was shrunk to, if one was declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<u64>,
}

/// A structured retrieval request over one or more datasources — the input to the `query`/`Search.run`
/// ops.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Query {
    /// The text to search for.
    pub find: String,
    /// An optional anchor the results should be near.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub near: Option<String>,
    /// An optional artifact/result type filter.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub ty: Option<String>,
    /// The datasources to search.
    #[serde(default)]
    pub sources: Vec<String>,
    /// An optional lower bound on result recency (ISO date).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// The maximum number of results to return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

/// A structured, evidence-bearing **successful** return from an agent task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Answer {
    /// A short status tag (e.g. `fixed`, `answered`).
    pub status: String,
    /// A human-readable summary of the outcome.
    pub summary: String,
    /// The evidence the answer is grounded in.
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    /// Any remaining gaps the answer did not fully close.
    #[serde(default)]
    pub gaps: Vec<String>,
    /// Residual risks the caller should be aware of.
    #[serde(default)]
    pub risks: Vec<String>,
}

/// A structured return signalling the task **could not** be completed, with the open gaps that blocked
/// it. Same shape as [`Answer`] but a distinct type so callers can branch on success vs. blockage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Blocked {
    /// A short status tag (e.g. `needs_work`, `blocked`).
    pub status: String,
    /// A human-readable summary of why the task is blocked.
    pub summary: String,
    /// Whatever evidence was gathered before blocking.
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    /// The gaps that blocked completion.
    #[serde(default)]
    pub gaps: Vec<String>,
    /// Risks surfaced while attempting the task.
    #[serde(default)]
    pub risks: Vec<String>,
}

/// A proposed code change — a concrete unified diff plus the path it applies to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Patch {
    /// The workspace-relative path the patch applies to.
    pub path: String,
    /// The unified diff (or semantic edit) to apply.
    pub diff: String,
}

/// The outcome of running a test command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TestResult {
    /// Whether the suite passed.
    pub ok: bool,
    /// The failing tests (empty when `ok`).
    #[serde(default)]
    pub failures: Vec<String>,
    /// A short summary line (counts, timing).
    #[serde(default)]
    pub summary: String,
}

/// A judge step's structured decision: the chosen outcome, the reasons behind it, and the evidence it
/// weighed. Consumed by the `ai.judge` cognition op.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Verdict {
    /// The chosen outcome (e.g. `supported`, `refuted`, `uncertain`).
    pub choice: String,
    /// The reasons behind the choice.
    #[serde(default)]
    pub reasons: Vec<String>,
    /// The evidence weighed in reaching the verdict.
    #[serde(default)]
    pub evidence: Vec<Evidence>,
}

/// A synthetic root referencing every v1-core prelude type, so a single `schema_for!` emits all of
/// their definitions under one `$defs` map (the shape [`prelude_schema`] returns).
#[derive(JsonSchema)]
#[allow(dead_code)]
struct PreludeRoot {
    span: Span,
    claim: Claim,
    evidence: Evidence,
    need: Need,
    ctx: Ctx,
    query: Query,
    answer: Answer,
    blocked: Blocked,
    patch: Patch,
    test_result: TestResult,
    verdict: Verdict,
}

/// The v1-core prelude type names, in catalog order. The schema `$defs` map may carry additional
/// *referenced* definitions (e.g. `ThingRef`); this list is the curated surface the catalog advertises.
pub const PRELUDE_TYPES: &[&str] = &[
    "Span",
    "Claim",
    "Evidence",
    "Need",
    "Ctx",
    "Query",
    "Answer",
    "Blocked",
    "Patch",
    "TestResult",
    "Verdict",
];

/// The `#/$defs` schema map of every prelude type (plus their referenced definitions) — the
/// definitions an op's `Named(...)` `$ref` resolves against. The artifact-ontology mirror of
/// [`crate::schema::ast_schema`].
pub fn prelude_schema() -> serde_json::Value {
    let root =
        serde_json::to_value(schemars::schema_for!(PreludeRoot)).expect("PreludeRoot schematizes");
    root.get("$defs")
        .or_else(|| root.get("definitions"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}))
}

/// A markdown `| type | description |` table of every prelude type, generated from the structs'
/// doc-comments — the SSOT mirror of [`crate::schema::node_kind_catalog`].
pub fn prelude_type_catalog() -> String {
    let defs = prelude_schema();
    let mut out = String::from("| type | description |\n|---|---|\n");
    for name in PRELUDE_TYPES {
        let desc = defs
            .get(name)
            .and_then(|d| d.get("description"))
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .replace('\n', " ");
        out.push_str(&format!("| `{name}` | {desc} |\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prelude_schema_defines_every_curated_type() {
        let defs = prelude_schema();
        for name in PRELUDE_TYPES {
            assert!(
                defs.get(name).is_some(),
                "prelude schema is missing a `$defs` entry for `{name}`"
            );
        }
        // Nested references are pulled in too (Span carries a ThingRef).
        assert!(
            defs.get("ThingRef").is_some(),
            "referenced definitions (ThingRef) are included for `$ref` resolution"
        );
    }

    #[test]
    fn prelude_type_catalog_covers_every_type_with_descriptions() {
        let catalog = prelude_type_catalog();
        assert!(catalog.starts_with("| type | description |\n|---|---|\n"));
        for name in PRELUDE_TYPES {
            assert!(
                catalog.contains(&format!("| `{name}` |")),
                "prelude catalog is missing `{name}`"
            );
        }
        // One row per type + 2 header lines, and descriptions flow from the doc-comments.
        assert_eq!(catalog.lines().count(), PRELUDE_TYPES.len() + 2);
        assert!(
            catalog.contains("claim"),
            "doc-comment descriptions are carried into the table"
        );
    }

    #[test]
    fn artifacts_are_plain_structs_round_tripping_through_json() {
        let claim = Claim {
            text: "tokens rejected after rotation".into(),
            span: None,
            confidence: 0.9,
        };
        let v = serde_json::to_value(&claim).unwrap();
        assert_eq!(v["text"], "tokens rejected after rotation");
        let back: Claim = serde_json::from_value(v).unwrap();
        assert_eq!(back, claim);
    }
}
