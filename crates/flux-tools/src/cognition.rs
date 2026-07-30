//! `cognition` — a pack of **pure** reasoning ops (no IO).
//!
//! These tools never touch the filesystem, network, or a process — they only transform their JSON
//! arguments and hand back a JSON string. Each declares an empty effect set (`effects: vec![]`),
//! `Risk::Low`, and `Idempotency::Idempotent`, so the runtime's policy/approval gates never fire for
//! them. They give the model a small, deterministic toolbox for shaping evidence: declaring what a
//! task `need`s, finding the `gaps` against gathered claims, and `compare`/`dedupe`/`sort`/`top`/
//! `merge`/`cite`/`len`/`first`/`last`/`filter` over lists of values.
//!
//! `review.normalize`/`review.aggregate` (strict-review Phase 3, `docs/designs/strict-review-flows.md`
//! "Aggregation") add a deterministic reviewer-output pipeline on top of the same primitives: parse
//! each reviewer's raw findings, quarantine malformed entries as `gaps` (never silently dropped, never
//! surfaced as findings), fingerprint by category/file/line/normalized-title, dedupe by fingerprint
//! (counting reviewer `agreement`), and rank by severity/confidence/agreement with a fingerprint
//! tiebreak so ordering is byte-identical across runs. Modeled on `flux-eval`'s
//! `improvements_aggregate` clustering (deterministic `BTreeMap`-free but same normalize/sort shape) —
//! not a dependency, just the same pattern.
//!
//! Every op is robust to missing optional params and wrong-typed input: it returns a clear
//! [`Error::Other`] rather than panicking.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{Idempotency, Risk, ToolSpec};

/// Register all pure cognition ops into a registry.
pub fn try_register_cognition(registry: &mut ToolRegistry) -> Result<()> {
    let mut assembled = registry.clone();
    assembled.try_register_all_from(
        "flux-tools pure cognition pack",
        vec![
            Arc::new(NeedTool) as Arc<dyn Tool>,
            Arc::new(GapsTool),
            Arc::new(CompareTool),
            Arc::new(TopTool),
            Arc::new(MergeTool),
            Arc::new(CiteTool),
            Arc::new(LenTool),
            Arc::new(FirstTool),
            Arc::new(LastTool),
        ],
    )?;
    crate::transform::try_register_transforms(&mut assembled)?;
    assembled.try_register_all_from(
        "flux-tools review and object cognition pack",
        vec![
            Arc::new(ReviewNormalizeTool) as Arc<dyn Tool>,
            Arc::new(ReviewAggregateTool),
            Arc::new(RegexMatchTool),
            Arc::new(RegexExtractTool),
            Arc::new(PickTool),
            Arc::new(OmitTool),
            Arc::new(MergeObjTool),
            Arc::new(CoalesceTool),
            Arc::new(KeysTool),
            Arc::new(ValuesTool),
            Arc::new(SumTool),
            Arc::new(CountByTool),
            Arc::new(GroupByTool),
            Arc::new(AnyTool),
            Arc::new(AllTool),
            Arc::new(HasTool),
        ],
    )?;
    *registry = assembled;
    Ok(())
}

/// Compatibility wrapper for pre-fallible pack installers.
///
/// # Deprecated
///
/// Production assembly should call [`try_register_cognition`].
pub fn register_cognition(registry: &mut ToolRegistry) {
    try_register_cognition(registry).expect("flux-tools cognition pack registration failed");
}

// ---------------------------------------------------------------------------
// shared helpers (pure)
// ---------------------------------------------------------------------------

/// Build the inert spec for a pure op: no effects, low risk, idempotent, no host access. Mirrors
/// the hand-written specs in `lib.rs` (no `flux-lang` dependency) but with an empty effect set so
/// the safety envelope treats the call as a no-IO transform.
fn pure_spec(name: &str, description: &str, input_schema: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema,
        output_schema: None,
        effects: vec![],
        risk: Risk::Low,
        idempotency: Idempotency::Idempotent,
        access: vec![],
        group: Some("cognition".into()),
    }
}

fn mark_flux_expr(mut schema: Value, params: &[&str]) -> Value {
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        for param in params {
            if let Some(prop) = properties.get_mut(*param).and_then(Value::as_object_mut) {
                prop.insert("format".into(), Value::String("flux-expr".into()));
            }
        }
    }
    schema
}

/// Fetch a required string param, or a clear error.
fn str_param<'a>(params: &'a Value, key: &str, tool: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Other(format!("{tool}: required string param `{key}` missing")))
}

/// Fetch a required array param (cloned), erroring if missing or not an array.
fn arr_param(params: &Value, key: &str, tool: &str) -> Result<Vec<Value>> {
    match params.get(key) {
        Some(Value::Array(a)) => Ok(a.clone()),
        None | Some(Value::Null) => Err(Error::Other(format!(
            "{tool}: required array param `{key}` missing"
        ))),
        Some(_) => Err(Error::Other(format!(
            "{tool}: param `{key}` must be an array"
        ))),
    }
}

/// Fetch an optional array param: missing/null yields an empty list; a non-array is an error.
fn arr_or_empty(params: &Value, key: &str, tool: &str) -> Result<Vec<Value>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => Ok(a.clone()),
        Some(_) => Err(Error::Other(format!(
            "{tool}: param `{key}` must be an array"
        ))),
    }
}

/// Truthiness for the `gaps` field-coverage heuristic: null/false/empty are falsy.
fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// The natural text of a claim: the string itself, or its `text` field.
fn claim_text(claim: &Value) -> Option<&str> {
    match claim {
        Value::String(s) => Some(s.as_str()),
        _ => claim.get("text").and_then(|v| v.as_str()),
    }
}

/// Collect an iterator of values, dropping later duplicates (whole-value equality), first-seen order.
fn dedup_keep(items: impl Iterator<Item = Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for it in items {
        if !out.contains(&it) {
            out.push(it);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// need
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct NeedInput {
    /// The question or goal to satisfy
    ask: String,
    /// Field names an answer must cover
    require: Vec<String>,
    /// Optional free-form completion predicate
    #[serde(default)]
    done_when: Option<Value>,
}

pub struct NeedTool;

#[async_trait]
impl Tool for NeedTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "need",
            "Construct a `Need` artifact `{ ask, require, done_when }` — the question to satisfy, \
             the field names an answer must cover, and an optional completion predicate. Pure: just \
             normalizes the inputs.",
            flux_spec::tool_input_schema::<NeedInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let ask = str_param(&params, "ask", "need")?.to_string();
        let require: Vec<String> = arr_param(&params, "require", "need")?
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect();
        let done_when = params.get("done_when").cloned().unwrap_or(Value::Null);
        let need = json!({ "ask": ask, "require": require, "done_when": done_when });
        Ok(ToolResult::ok(serde_json::to_string(&need)?))
    }
}

// ---------------------------------------------------------------------------
// gaps
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GapsInput {
    /// Evidence claims gathered so far
    claims: Vec<Value>,
    /// The need whose `require` fields are checked
    need: Value,
}

pub struct GapsTool;

#[async_trait]
impl Tool for GapsTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "gaps",
            "Given `claims` and a `need`, return a JSON array of the `need.require` field names not \
             yet covered. Heuristic (v1): a field is covered if any claim's `text` contains the \
             field name (case-insensitive) OR a claim has a truthy field of that name.",
            flux_spec::tool_input_schema::<GapsInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let claims = arr_or_empty(&params, "claims", "gaps")?;
        let require: Vec<String> = match params.get("need") {
            None | Some(Value::Null) => {
                return Err(Error::Other("gaps: required param `need` missing".into()))
            }
            Some(Value::Object(_)) => params
                .get("need")
                .and_then(|n| n.get("require"))
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            Some(_) => return Err(Error::Other("gaps: param `need` must be an object".into())),
        };

        let unmet: Vec<String> = require
            .into_iter()
            .filter(|field| {
                if field.is_empty() {
                    return false; // an empty require field is vacuously covered (malformed input)
                }
                let needle = field.to_lowercase();
                let covered = claims.iter().any(|c| {
                    let text_hit = claim_text(c)
                        .map(|t| t.to_lowercase().contains(&needle))
                        .unwrap_or(false);
                    // Field-presence check is case-insensitive too (mirrors the text path): a claim
                    // with a truthy key equal to `field` (ignoring case) covers it.
                    let field_hit = c
                        .as_object()
                        .map(|o| {
                            o.iter()
                                .any(|(k, v)| k.eq_ignore_ascii_case(field) && is_truthy(v))
                        })
                        .unwrap_or(false);
                    text_hit || field_hit
                });
                !covered
            })
            .collect();
        Ok(ToolResult::ok(serde_json::to_string(&unmet)?))
    }
}

// ---------------------------------------------------------------------------
// compare
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CompareInput {
    /// Baseline list
    a: Vec<Value>,
    /// Candidate list
    b: Vec<Value>,
}

pub struct CompareTool;

#[async_trait]
impl Tool for CompareTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "compare",
            "Compare two arrays by JSON equality, returning `{ added, removed, common }`: items in \
             `b` but not `a`, in `a` but not `b`, and in both (each de-duplicated, first-seen order).",
            flux_spec::tool_input_schema::<CompareInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let a = arr_or_empty(&params, "a", "compare")?;
        let b = arr_or_empty(&params, "b", "compare")?;
        let removed = dedup_keep(a.iter().filter(|x| !b.iter().any(|y| y == *x)).cloned());
        let added = dedup_keep(b.iter().filter(|x| !a.iter().any(|y| y == *x)).cloned());
        let common = dedup_keep(a.iter().filter(|x| b.iter().any(|y| y == *x)).cloned());
        let out = json!({ "added": added, "removed": removed, "common": common });
        Ok(ToolResult::ok(serde_json::to_string(&out)?))
    }
}

// ---------------------------------------------------------------------------
// top
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct TopInput {
    items: Vec<Value>,
    /// Number of leading items to keep
    n: u64,
}

pub struct TopTool;

#[async_trait]
impl Tool for TopTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "top",
            "Return the first `n` items of an array (fewer if the array is shorter).",
            flux_spec::tool_input_schema::<TopInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = arr_or_empty(&params, "items", "top")?;
        let n = params.get("n").and_then(|v| v.as_u64()).ok_or_else(|| {
            Error::Other("top: required non-negative integer param `n` missing".into())
        })? as usize;
        let out: Vec<Value> = items.into_iter().take(n).collect();
        Ok(ToolResult::ok(serde_json::to_string(&out)?))
    }
}

// ---------------------------------------------------------------------------
// merge
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MergeInput {
    /// The lists to concatenate, in order
    lists: Vec<Vec<Value>>,
}

pub struct MergeTool;

#[async_trait]
impl Tool for MergeTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "merge",
            "Concatenate an array-of-arrays into a single array, in order.",
            flux_spec::tool_input_schema::<MergeInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let lists = arr_param(&params, "lists", "merge")?;
        let mut out: Vec<Value> = Vec::new();
        for (i, l) in lists.into_iter().enumerate() {
            // A string element that IS a JSON array (values are stored as JSON strings) counts —
            // the same string-leaf re-parse rule the runtime's templates apply (C-10).
            let l = parse_json_array_string(l);
            match l {
                Value::Array(a) => out.extend(a),
                // An ABSENT list contributes nothing: `null` or the empty-string "absent" idiom (what
                // an optional `$x.field?` read of a missing list binds to) is treated as `[]`, so a
                // fan-out where some branches produced no list still merges the rest instead of
                // hard-erroring. A genuine non-array value (e.g. a number, or a non-array string) is
                // still a type error.
                Value::Null => {}
                Value::String(ref s) if s.is_empty() => {}
                _ => {
                    return Err(Error::Other(format!(
                        "merge: element {i} of `lists` is not an array"
                    )))
                }
            }
        }
        Ok(ToolResult::ok(serde_json::to_string(&out)?))
    }
}

/// Re-parse a string that holds a serialized JSON array (the store's JSON-as-string form) into the
/// array itself; anything else passes through unchanged.
fn parse_json_array_string(v: Value) -> Value {
    if let Value::String(s) = &v {
        if let Ok(parsed) = serde_json::from_str::<Value>(s) {
            if parsed.is_array() {
                return parsed;
            }
        }
    }
    v
}

// ---------------------------------------------------------------------------
// len
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
enum LenItems {
    String(String),
    Array(Vec<Value>),
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LenInput {
    /// An array (length) or a string (character count)
    items: LenItems,
}

pub struct LenTool;

#[async_trait]
impl Tool for LenTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "len",
            "Return the number of items in an array (or the character count of a string). \
             Use with `when`/`expr` to branch on list size without shelling out.",
            flux_spec::tool_input_schema::<LenInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        match params.get("items") {
            Some(Value::Array(a)) => Ok(ToolResult::ok(a.len().to_string())),
            Some(Value::String(s)) => Ok(ToolResult::ok(s.chars().count().to_string())),
            None | Some(Value::Null) => {
                Err(Error::Other("len: required param `items` missing".into()))
            }
            Some(_) => Err(Error::Other(
                "len: param `items` must be an array or a string".into(),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// first / last
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FirstInput {
    items: Vec<Value>,
}

pub struct FirstTool;

#[async_trait]
impl Tool for FirstTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "first",
            "Return the first item of an array (or `null` if the array is empty).",
            flux_spec::tool_input_schema::<FirstInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = arr_param(&params, "items", "first")?;
        let v = items.into_iter().next().unwrap_or(Value::Null);
        Ok(ToolResult::ok(serde_json::to_string(&v)?))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LastInput {
    items: Vec<Value>,
}

pub struct LastTool;

#[async_trait]
impl Tool for LastTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "last",
            "Return the last item of an array (or `null` if the array is empty).",
            flux_spec::tool_input_schema::<LastInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = arr_param(&params, "items", "last")?;
        let v = items.into_iter().next_back().unwrap_or(Value::Null);
        Ok(ToolResult::ok(serde_json::to_string(&v)?))
    }
}

// ---------------------------------------------------------------------------
// review.normalize / review.aggregate — strict-review Phase 3 typed artifacts
// (docs/designs/strict-review-flows.md "Review artifacts" + "Aggregation")
// ---------------------------------------------------------------------------

/// A single review finding — the strict-review protocol's typed unit of feedback. Embedded schema
/// first (this story); promotion to `flux_lang::prelude::PRELUDE_TYPES` is deferred until a second
/// surface consumes it.
#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema, Clone)]
pub struct ReviewFinding {
    /// Stable fingerprint computed from category+file+line+normalized title (never model-supplied)
    pub fingerprint: String,
    /// "critical" | "high" | "medium" | "low" | "info"
    pub severity: String,
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
    pub title: String,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub recommendation: String,
    #[serde(default)]
    pub confidence: f64,
    /// The reviewer that raised this finding (or the first, for a collapsed duplicate).
    #[serde(default)]
    pub reviewer: String,
    /// Number of distinct reviewers that raised this fingerprint (>1 after dedupe collapses agreeing
    /// reviewers into one finding).
    #[serde(default = "one")]
    pub agreement: u32,
}

fn one() -> u32 {
    1
}

/// The full aggregated review artifact `review.aggregate` returns.
#[allow(dead_code)]
#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ReviewReport {
    /// Short generated summary (counts).
    pub summary: String,
    /// Deduped, ranked findings (severity desc, then confidence desc, then agreement desc, then
    /// fingerprint asc as a stable tiebreak).
    pub findings: Vec<ReviewFinding>,
    pub checked_files: Vec<String>,
    pub reviewers: Vec<String>,
    /// Human-readable descriptions of malformed reviewer entries that were quarantined rather than
    /// silently dropped or surfaced as findings.
    pub gaps: Vec<String>,
}

/// Numeric rank for severity ordering (higher = more severe): critical>high>medium>low>info. An
/// unrecognized/missing severity sorts as the lowest tier (below "info") rather than erroring —
/// malformed severities are still ranked deterministically, never panicking.
fn severity_rank(sev: &str) -> u8 {
    match sev {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        "info" => 0,
        _ => 0,
    }
}

const REQUIRED_SEVERITIES: &[&str] = &["critical", "high", "medium", "low", "info"];

/// Normalize a title the same way `flux-eval`'s `aggregate.rs::normalize` does: lowercase,
/// alphanumeric-only (other chars become a space), collapse whitespace — but joined with spaces
/// (not hyphens) since this feeds a fingerprint string, not an id slug.
fn normalize_title(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Deterministic fingerprint: category + file + line + normalized title, joined by a separator that
/// cannot naturally appear in any part (so distinct inputs cannot collide by concatenation), then
/// stably hashed. Same inputs -> same fingerprint on every run (no randomness, no HashMap iteration
/// order, no wall-clock).
fn compute_fingerprint(
    category: &str,
    file: Option<&str>,
    line: Option<i64>,
    title: &str,
) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let key = format!(
        "{category}\u{1}{}\u{1}{}\u{1}{}",
        file.unwrap_or(""),
        line.map(|l| l.to_string()).unwrap_or_default(),
        normalize_title(title)
    );
    // `DefaultHasher` (SipHash-1-3 with a fixed zero key) is deterministic across runs and processes
    // for a given input — it is only randomized per-`HashMap` via `RandomState`, which this bypasses
    // by constructing the hasher directly.
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Parse one raw reviewer entry into a `ReviewFinding`, or a human-readable gap description if it is
/// malformed (not an object, or missing a required string field, or an unrecognized `severity`).
/// Never panics; never silently drops — every malformed entry becomes exactly one gap string.
fn parse_finding(raw: &Value) -> std::result::Result<ReviewFinding, String> {
    let obj = raw
        .as_object()
        .ok_or_else(|| format!("dropped malformed finding: not an object: {}", compact(raw)))?;

    let title = obj
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!(
                "dropped malformed finding: missing `title`: {}",
                compact(raw)
            )
        })?;
    let category = obj
        .get("category")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!(
                "dropped malformed finding: missing `category`: {}",
                compact(raw)
            )
        })?;
    let severity = obj
        .get("severity")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!(
                "dropped malformed finding: missing `severity`: {}",
                compact(raw)
            )
        })?;
    if !REQUIRED_SEVERITIES.contains(&severity) {
        return Err(format!(
            "dropped malformed finding: invalid `severity` {:?} (want one of {REQUIRED_SEVERITIES:?}): {}",
            severity,
            compact(raw)
        ));
    }

    let file = obj.get("file").and_then(|v| v.as_str()).map(String::from);
    let line = obj.get("line").and_then(|v| v.as_i64());
    let evidence = obj
        .get("evidence")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let recommendation = obj
        .get("recommendation")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let confidence = obj
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let reviewer = obj
        .get("reviewer")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let fingerprint = compute_fingerprint(category, file.as_deref(), line, title);

    Ok(ReviewFinding {
        fingerprint,
        severity: severity.to_string(),
        category: category.to_string(),
        file,
        line,
        title: title.to_string(),
        evidence,
        recommendation,
        confidence,
        reviewer,
        agreement: 1,
    })
}

/// A short, capped rendering of a JSON value for a gap message (avoid dumping huge payloads).
fn compact(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 200 {
        let head: String = s.chars().take(200).collect();
        format!("{head}…")
    } else {
        s
    }
}

/// `review.normalize`: parse each raw reviewer entry into a well-formed `ReviewFinding` (computing
/// its fingerprint), quarantining malformed entries as `gaps`. Does NOT dedupe or rank — that is
/// `review.aggregate`'s job. Pure and order-preserving (first-seen order retained).
///
/// Entries come in three shapes (L-14): a finding **object** (parsed directly), an **array** (a
/// whole reviewer output — flattened), or a **string** (a raw reviewer blob: real reviewers wrap
/// their JSON in ```fences``` or prose despite instructions). Strings are recovered leniently; an
/// unrecoverable blob becomes ONE quarantined gap — a sloppy reviewer degrades the report, it never
/// aborts the flow after the sub-agent spend.
fn normalize_findings(raw: &[Value]) -> (Vec<ReviewFinding>, Vec<String>) {
    let mut findings = Vec::with_capacity(raw.len());
    let mut gaps = Vec::new();
    for entry in raw {
        normalize_entry(entry, &mut findings, &mut gaps);
    }
    (findings, gaps)
}

fn normalize_entry(entry: &Value, findings: &mut Vec<ReviewFinding>, gaps: &mut Vec<String>) {
    match entry {
        Value::Array(items) => {
            for item in items {
                normalize_entry(item, findings, gaps);
            }
        }
        Value::String(s) => match parse_reviewer_blob(s) {
            Some(v) => normalize_entry(&v, findings, gaps),
            None => gaps.push(format!(
                "unparseable reviewer output (quarantined): {}",
                s.chars().take(200).collect::<String>()
            )),
        },
        other => match parse_finding(other) {
            Ok(f) => findings.push(f),
            Err(gap) => gaps.push(gap),
        },
    }
}

/// Leniently recover the JSON from a raw reviewer blob: an as-is parse first, then with a
/// ```json``` / ``` ``` code fence stripped, then the first `[...]` array slice in the text
/// (prose-wrapped output). `None` = nothing recoverable.
fn parse_reviewer_blob(s: &str) -> Option<Value> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Some(v);
    }
    if let Some(fenced) = strip_code_fence(t) {
        if let Ok(v) = serde_json::from_str::<Value>(fenced.trim()) {
            return Some(v);
        }
    }
    if let (Some(start), Some(end)) = (t.find('['), t.rfind(']')) {
        if start < end {
            if let Ok(v) = serde_json::from_str::<Value>(&t[start..=end]) {
                return Some(v);
            }
        }
    }
    None
}

/// The content of the first ``` code fence in `t` (tolerating a language tag on the opening line),
/// or `None` when there is no complete fence.
fn strip_code_fence(t: &str) -> Option<&str> {
    let open = t.find("```")?;
    let after_tag = t[open + 3..].find('\n').map(|n| open + 3 + n + 1)?;
    let close = t[after_tag..].find("```")?;
    Some(&t[after_tag..after_tag + close])
}

/// Dedupe by fingerprint, merging duplicates into one finding whose `agreement` counts the number of
/// DISTINCT `reviewer`s that raised it (an empty/blank reviewer string counts as one distinct source
/// per occurrence — it never merges with a named reviewer). Keeps the first-seen finding's fields
/// (title/evidence/recommendation/etc.) as the representative; `confidence` becomes the max across the
/// group (the most confident reviewer's assessment wins), matching "rank by severity, confidence,
/// agreement" using the strongest signal available.
fn dedupe_by_fingerprint(findings: Vec<ReviewFinding>) -> Vec<ReviewFinding> {
    use std::collections::BTreeMap;
    // BTreeMap keyed by fingerprint keeps the merge deterministic and independent of any hashmap
    // iteration order; insertion order within a bucket is preserved via the `Vec` we build first.
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<ReviewFinding>> = BTreeMap::new();
    for f in findings {
        if !groups.contains_key(&f.fingerprint) {
            order.push(f.fingerprint.clone());
        }
        groups.entry(f.fingerprint.clone()).or_default().push(f);
    }

    order
        .into_iter()
        .map(|fp| {
            let group = groups.remove(&fp).unwrap_or_default();
            let mut reviewers: Vec<&str> = group
                .iter()
                .map(|f| f.reviewer.as_str())
                .filter(|r| !r.is_empty())
                .collect();
            reviewers.sort_unstable();
            reviewers.dedup();
            // Distinct non-empty reviewers, plus one occurrence per blank-reviewer entry (each an
            // independently-unnamed source rather than a shared identity).
            let blank_occurrences = group.iter().filter(|f| f.reviewer.is_empty()).count();
            let agreement = (reviewers.len() + blank_occurrences).max(1) as u32;
            let confidence = group.iter().map(|f| f.confidence).fold(f64::MIN, f64::max);
            let mut rep = group.into_iter().next().expect("group is non-empty");
            rep.confidence = if confidence.is_finite() {
                confidence
            } else {
                rep.confidence
            };
            rep.agreement = agreement;
            rep
        })
        .collect()
}

/// Rank findings deterministically: severity desc, then confidence desc, then agreement desc, then
/// fingerprint asc as a final stable tiebreak — so ordering is byte-identical across runs regardless
/// of input order or platform (`sort_by` is stable, but the explicit fingerprint tiebreak means the
/// comparator alone fully orders any two distinct findings, so stability doesn't even need to be
/// relied on).
fn rank_findings(mut findings: Vec<ReviewFinding>) -> Vec<ReviewFinding> {
    findings.sort_by(|a, b| {
        severity_rank(&b.severity)
            .cmp(&severity_rank(&a.severity))
            .then_with(|| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| b.agreement.cmp(&a.agreement))
            .then_with(|| a.fingerprint.cmp(&b.fingerprint))
    });
    findings
}

/// Arguments for `review.normalize`.
#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReviewNormalizeInput {
    /// Raw reviewer output entries (each SHOULD be a finding object)
    findings: Vec<Value>,
}

pub struct ReviewNormalizeTool;

#[async_trait]
impl Tool for ReviewNormalizeTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "review.normalize",
            "Parse raw reviewer output into well-formed findings, computing each finding's stable \
             fingerprint (category+file+line+normalized-title) and quarantining malformed entries as \
             human-readable `gaps` strings instead of silently dropping or surfacing them as \
             findings. Returns `{ findings, gaps }`. Does not dedupe or rank — see `review.aggregate`.",
            flux_spec::tool_input_schema::<ReviewNormalizeInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let raw = arr_or_empty(&params, "findings", "review.normalize")?;
        let (findings, gaps) = normalize_findings(&raw);
        let out = json!({ "findings": findings, "gaps": gaps });
        Ok(ToolResult::ok(serde_json::to_string(&out)?))
    }
}

/// Arguments for `review.aggregate`.
#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReviewAggregateInput {
    /// Raw reviewer output entries (each SHOULD be a finding object)
    findings: Vec<Value>,
    /// Files that were checked (echoed into the report as `checked_files`)
    #[serde(default)]
    files: Vec<String>,
    /// Reviewer names that participated (echoed into the report as `reviewers`)
    #[serde(default)]
    reviewers: Vec<String>,
}

pub struct ReviewAggregateTool;

#[async_trait]
impl Tool for ReviewAggregateTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "review.aggregate",
            "Deterministically aggregate strict-review reviewer output into a full `ReviewReport`: \
             normalizes raw findings (quarantining malformed entries as `gaps`), dedupes by \
             fingerprint (counting distinct-reviewer `agreement`), and ranks by severity, then \
             confidence, then agreement, with a fingerprint tiebreak for byte-identical ordering \
             across runs. Returns `{ summary, findings, checked_files, reviewers, gaps }`.",
            flux_spec::tool_input_schema::<ReviewAggregateInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let raw = arr_or_empty(&params, "findings", "review.aggregate")?;
        let files: Vec<String> = arr_or_empty(&params, "files", "review.aggregate")?
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        let reviewers: Vec<String> = arr_or_empty(&params, "reviewers", "review.aggregate")?
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();

        let (parsed, gaps) = normalize_findings(&raw);
        let deduped = dedupe_by_fingerprint(parsed);
        let ranked = rank_findings(deduped);

        let summary = format!(
            "strict review of {} file(s): {} ranked finding(s) from {} reviewer(s) ({} raw, {} gap(s))",
            files.len(),
            ranked.len(),
            reviewers.len(),
            raw.len(),
            gaps.len()
        );

        let report = ReviewReport {
            summary,
            findings: ranked,
            checked_files: files,
            reviewers,
            gaps,
        };
        Ok(ToolResult::ok(serde_json::to_string(&report)?))
    }
}

// ---------------------------------------------------------------------------
// cite
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CiteInput {
    /// Claims to cite
    claims: Vec<Value>,
}

pub struct CiteTool;

/// Build the trailing `(source/span)` part of a citation line, or empty if neither is present.
fn cite_suffix(source: Option<&str>, span: Option<&Value>) -> String {
    let span_str = match span {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(v) => Some(v.to_string()),
    };
    match (source, span_str) {
        (Some(s), Some(sp)) => format!(" ({s}: {sp})"),
        (Some(s), None) => format!(" ({s})"),
        (None, Some(sp)) => format!(" ({sp})"),
        (None, None) => String::new(),
    }
}

#[async_trait]
impl Tool for CiteTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "cite",
            "Render claims as a markdown citation list — one line per claim: \
             `- \"<text>\" (<source/span if present>)`.",
            flux_spec::tool_input_schema::<CiteInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let claims = arr_or_empty(&params, "claims", "cite")?;
        let lines: Vec<String> = claims
            .iter()
            .map(|c| {
                let text = claim_text(c).unwrap_or("");
                let source = c.get("source").and_then(|v| v.as_str());
                let span = c.get("span");
                format!("- \"{text}\"{}", cite_suffix(source, span))
            })
            .collect();
        Ok(ToolResult::ok(lines.join("\n")))
    }
}

// ---------------------------------------------------------------------------
// regex_match
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RegexMatchInput {
    /// The string to search in
    s: String,
    /// The regex pattern to match against
    pattern: String,
}

pub struct RegexMatchTool;

#[async_trait]
impl Tool for RegexMatchTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "regex_match",
            "Test if a string matches a regex pattern, returning \"true\" or \"false\" (strings, \
             matching the boolean-emitter convention). Uses Rust's `regex` crate (Thompson NFA, \
             linear-time, ReDoS-free). Pattern length > 512 chars or invalid patterns yield clear \
             errors.",
            flux_spec::tool_input_schema::<RegexMatchInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let s = str_param(&params, "s", "regex_match")?.to_string();
        let pattern = str_param(&params, "pattern", "regex_match")?;

        // Check pattern length before compiling
        if pattern.len() > 512 {
            return Err(Error::Other(
                "regex_match: pattern exceeds 512 chars".into(),
            ));
        }

        // Compile with size limit to prevent pathological patterns
        let regex = regex::RegexBuilder::new(pattern)
            .size_limit(1_048_576)
            .build()
            .map_err(|e| Error::Other(format!("regex_match: invalid pattern: {e}")))?;

        let matched = regex.is_match(&s);
        Ok(ToolResult::ok(if matched { "true" } else { "false" }))
    }
}

// ---------------------------------------------------------------------------
// regex_extract
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RegexExtractInput {
    /// The string to search in
    s: String,
    /// The regex pattern to match against
    pattern: String,
    /// Capture group index to extract (default 0 = whole match)
    #[serde(default)]
    group: Option<usize>,
    /// Extract all matches instead of just the first (default false)
    #[serde(default)]
    all: Option<bool>,
}

pub struct RegexExtractTool;

#[async_trait]
impl Tool for RegexExtractTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "regex_extract",
            "Extract text from a string using a regex pattern. Returns the first match as a string, \
             or `null` if no match. With `group` (default 0 = whole match), extracts that capture \
             group. With `all: true`, returns a JSON array of all matches. Missing capture group \
             index yields a clear error.",
            flux_spec::tool_input_schema::<RegexExtractInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let s = str_param(&params, "s", "regex_extract")?.to_string();
        let pattern = str_param(&params, "pattern", "regex_extract")?;
        let group = params
            .get("group")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(0);
        let all = params.get("all").and_then(|v| v.as_bool()).unwrap_or(false);

        // Check pattern length before compiling
        if pattern.len() > 512 {
            return Err(Error::Other(
                "regex_extract: pattern exceeds 512 chars".into(),
            ));
        }

        // Compile with size limit
        let regex = regex::RegexBuilder::new(pattern)
            .size_limit(1_048_576)
            .build()
            .map_err(|e| Error::Other(format!("regex_extract: invalid pattern: {e}")))?;

        if all {
            // Extract all matches of the requested group
            let mut matches = Vec::new();
            for caps in regex.captures_iter(&s) {
                if group > 0 {
                    match caps.get(group) {
                        Some(m) => matches.push(m.as_str().to_string()),
                        None => {
                            return Err(Error::Other(format!(
                                "regex_extract: no capture group {group} in pattern"
                            )))
                        }
                    }
                } else {
                    // group 0 is the whole match, always available if captures matched
                    if let Some(m) = caps.get(0) {
                        matches.push(m.as_str().to_string());
                    }
                }
            }
            Ok(ToolResult::ok(serde_json::to_string(&matches)?))
        } else {
            // Extract first match of the requested group
            match regex.captures(&s) {
                None => Ok(ToolResult::ok("null".to_string())),
                Some(caps) => {
                    if group > 0 {
                        match caps.get(group) {
                            Some(m) => Ok(ToolResult::ok(serde_json::to_string(m.as_str())?)),
                            None => Err(Error::Other(format!(
                                "regex_extract: no capture group {group} in pattern"
                            ))),
                        }
                    } else {
                        // group 0 always available
                        match caps.get(0) {
                            Some(m) => Ok(ToolResult::ok(serde_json::to_string(m.as_str())?)),
                            None => Ok(ToolResult::ok("null".to_string())),
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// pick / omit / merge_obj / coalesce / keys / values (L-49)
// ---------------------------------------------------------------------------

/// Helper: apply pick/omit to a single object.
fn pick_omit_single(obj: &Value, keys: &[String], keep: bool) -> Result<Value> {
    let map = obj
        .as_object()
        .ok_or_else(|| Error::Other("item is not an object".into()))?;
    let mut out = serde_json::Map::new();
    for (k, v) in map {
        let should_keep = keys.iter().any(|key| key == k);
        if (keep && should_keep) || (!keep && !should_keep) {
            out.insert(k.clone(), v.clone());
        }
    }
    Ok(Value::Object(out))
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PickInput {
    /// One object or an array of objects
    items: Value,
    /// Field names to keep
    keys: Vec<String>,
}

pub struct PickTool;

#[async_trait]
impl Tool for PickTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "pick",
            "Keep only the listed keys from an object (or per element of an array of objects). \
             Missing keys simply don't appear. Returns an object if `items` is an object, or an \
             array if `items` is an array.",
            flux_spec::tool_input_schema::<PickInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = params
            .get("items")
            .ok_or_else(|| Error::Other("pick: required param `items` missing".into()))?;
        let keys = arr_param(&params, "keys", "pick")?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect::<Vec<_>>();

        let result = match items {
            Value::Object(_) => pick_omit_single(items, &keys, true)?,
            Value::Array(arr) => {
                let picked: Result<Vec<Value>> = arr
                    .iter()
                    .map(|obj| pick_omit_single(obj, &keys, true))
                    .collect();
                Value::Array(picked?)
            }
            _ => {
                return Err(Error::Other(
                    "pick: param `items` must be an object or an array of objects".into(),
                ))
            }
        };
        Ok(ToolResult::ok(serde_json::to_string(&result)?))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct OmitInput {
    /// One object or an array of objects
    items: Value,
    /// Field names to remove
    keys: Vec<String>,
}

pub struct OmitTool;

#[async_trait]
impl Tool for OmitTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "omit",
            "Remove the listed keys from an object (or per element of an array of objects), \
             keeping everything else. Returns an object if `items` is an object, or an array if \
             `items` is an array.",
            flux_spec::tool_input_schema::<OmitInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = params
            .get("items")
            .ok_or_else(|| Error::Other("omit: required param `items` missing".into()))?;
        let keys = arr_param(&params, "keys", "omit")?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect::<Vec<_>>();

        let result = match items {
            Value::Object(_) => pick_omit_single(items, &keys, false)?,
            Value::Array(arr) => {
                let omitted: Result<Vec<Value>> = arr
                    .iter()
                    .map(|obj| pick_omit_single(obj, &keys, false))
                    .collect();
                Value::Array(omitted?)
            }
            _ => {
                return Err(Error::Other(
                    "omit: param `items` must be an object or an array of objects".into(),
                ))
            }
        };
        Ok(ToolResult::ok(serde_json::to_string(&result)?))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MergeObjInput {
    /// Array of objects to shallow-merge (later keys win)
    objects: Vec<Value>,
}

pub struct MergeObjTool;

#[async_trait]
impl Tool for MergeObjTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "merge_obj",
            "Shallow-merge an array of objects left-to-right (later keys win). Returns one merged \
             object. Distinct from the list-concat `merge` tool.",
            flux_spec::tool_input_schema::<MergeObjInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let objects = arr_param(&params, "objects", "merge_obj")?;
        let mut merged = serde_json::Map::new();
        for (i, obj) in objects.into_iter().enumerate() {
            match obj.as_object() {
                Some(map) => {
                    for (k, v) in map {
                        merged.insert(k.clone(), v.clone());
                    }
                }
                None => {
                    return Err(Error::Other(format!(
                        "merge_obj: element {i} is not an object"
                    )))
                }
            }
        }
        Ok(ToolResult::ok(serde_json::to_string(&Value::Object(
            merged,
        ))?))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CoalesceInput {
    /// Array of candidate values
    values: Vec<Value>,
    /// Optional default if nothing qualifies
    #[serde(default)]
    default: Option<Value>,
}

pub struct CoalesceTool;

fn is_empty_for_coalesce(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        _ => false,
    }
}

#[async_trait]
impl Tool for CoalesceTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "coalesce",
            "Return the first value that is not `null` and not `\"\"` (empty string). Zero and \
             false ARE kept. If nothing qualifies, return `default` if provided, else `null`.",
            flux_spec::tool_input_schema::<CoalesceInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let values = arr_param(&params, "values", "coalesce")?;
        let default = params.get("default").cloned().unwrap_or(Value::Null);

        let chosen = values
            .into_iter()
            .find(|v| !is_empty_for_coalesce(v))
            .unwrap_or(default);
        Ok(ToolResult::ok(serde_json::to_string(&chosen)?))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct KeysInput {
    /// An object
    item: Value,
}

pub struct KeysTool;

#[async_trait]
impl Tool for KeysTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "keys",
            "Return the keys of an object as a JSON array of strings, in deterministic order.",
            flux_spec::tool_input_schema::<KeysInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let item = params
            .get("item")
            .ok_or_else(|| Error::Other("keys: required param `item` missing".into()))?;
        match item.as_object() {
            Some(map) => {
                let keys: Vec<String> = map.keys().cloned().collect();
                Ok(ToolResult::ok(serde_json::to_string(&keys)?))
            }
            None => Err(Error::Other("keys: param `item` must be an object".into())),
        }
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ValuesInput {
    /// An object
    item: Value,
}

pub struct ValuesTool;

#[async_trait]
impl Tool for ValuesTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "values",
            "Return the values of an object as a JSON array, in the same order as `keys`.",
            flux_spec::tool_input_schema::<ValuesInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let item = params
            .get("item")
            .ok_or_else(|| Error::Other("values: required param `item` missing".into()))?;
        match item.as_object() {
            Some(map) => {
                let values: Vec<Value> = map.values().cloned().collect();
                Ok(ToolResult::ok(serde_json::to_string(&values)?))
            }
            None => Err(Error::Other(
                "values: param `item` must be an object".into(),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// sum / count_by / group_by / any / all / has (L-48)
// ---------------------------------------------------------------------------

/// Extract a value at a dotted path from an object.
fn extract_dotted_path(obj: &Value, path: &str) -> Value {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = obj.clone();
    for part in parts {
        current = current.get(part).cloned().unwrap_or(Value::Null);
    }
    current
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SumInput {
    items: Vec<Value>,
    #[serde(default)]
    path: Option<String>,
}

pub struct SumTool;

#[async_trait]
impl Tool for SumTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "sum",
            "Sum numeric values in an array. If `path` is provided, extracts that dotted field from \
             each element and sums the extracted values. Non-numeric elements yield a clear error.",
            flux_spec::tool_input_schema::<SumInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = arr_or_empty(&params, "items", "sum")?;
        let path = params.get("path").and_then(|v| v.as_str());

        let mut total = 0.0;
        for (i, item) in items.iter().enumerate() {
            let val = match path {
                Some(p) => extract_dotted_path(item, p),
                None => item.clone(),
            };
            match val.as_f64() {
                Some(n) => total += n,
                None => {
                    return Err(Error::Other(format!(
                        "sum: element {i} is not numeric (got {})",
                        serde_json::to_string(&val).unwrap_or_else(|_| "?".into())
                    )))
                }
            }
        }

        Ok(ToolResult::ok(flux_lang::expr::format_number(total)))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CountByInput {
    items: Vec<Value>,
    path: String,
}

pub struct CountByTool;

#[async_trait]
impl Tool for CountByTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "count_by",
            "Group array items by a field and count each group. Returns `[{key, count}]` sorted by \
             count descending, then key ascending (deterministic).",
            flux_spec::tool_input_schema::<CountByInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = arr_or_empty(&params, "items", "count_by")?;
        let path = str_param(&params, "path", "count_by")?;

        use std::collections::BTreeMap;
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for item in items {
            let val = extract_dotted_path(&item, path);
            let key = match &val {
                Value::String(s) => s.clone(),
                other => serde_json::to_string(other)?,
            };
            *counts.entry(key).or_insert(0) += 1;
        }

        let mut results: Vec<Value> = counts
            .into_iter()
            .map(|(key, count)| json!({"key": key, "count": count}))
            .collect();

        results.sort_by(|a, b| {
            let count_a = a["count"].as_u64().unwrap_or(0);
            let count_b = b["count"].as_u64().unwrap_or(0);
            count_b.cmp(&count_a).then_with(|| {
                let key_a = a["key"].as_str().unwrap_or("");
                let key_b = b["key"].as_str().unwrap_or("");
                key_a.cmp(key_b)
            })
        });

        Ok(ToolResult::ok(serde_json::to_string(&results)?))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GroupByInput {
    items: Vec<Value>,
    path: String,
}

pub struct GroupByTool;

#[async_trait]
impl Tool for GroupByTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "group_by",
            "Group array items by a field. Returns `[{key, items}]` in first-seen key order.",
            flux_spec::tool_input_schema::<GroupByInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = arr_or_empty(&params, "items", "group_by")?;
        let path = str_param(&params, "path", "group_by")?;

        let mut order: Vec<String> = Vec::new();
        let mut groups: std::collections::HashMap<String, Vec<Value>> =
            std::collections::HashMap::new();

        for item in items {
            let val = extract_dotted_path(&item, path);
            let key = match &val {
                Value::String(s) => s.clone(),
                other => serde_json::to_string(other)?,
            };
            if !groups.contains_key(&key) {
                order.push(key.clone());
            }
            groups.entry(key).or_default().push(item);
        }

        let results: Vec<Value> = order
            .into_iter()
            .map(|key| {
                let items = groups.remove(&key).unwrap_or_default();
                json!({"key": key, "items": items})
            })
            .collect();

        Ok(ToolResult::ok(serde_json::to_string(&results)?))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AnyInput {
    items: Vec<Value>,
    #[serde(default, rename = "where")]
    where_formula: Option<String>,
    #[serde(default)]
    vars: Option<serde_json::Map<String, Value>>,
}

pub struct AnyTool;

#[async_trait]
impl Tool for AnyTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "any",
            "Returns \"true\" if any element satisfies the predicate, \"false\" otherwise. With \
             `where`, evaluates the formula per element with `it` bound. Without `where`, checks if \
             any element is truthy. Empty list → \"false\".",
            mark_flux_expr(flux_spec::tool_input_schema::<AnyInput>(), &["where"]),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        use flux_lang::expr::{eval_expr_value, ExprVal};

        let items = arr_or_empty(&params, "items", "any")?;
        let where_formula = params.get("where").and_then(|v| v.as_str());

        let satisfied = items.iter().any(|element| {
            if let Some(formula) = where_formula {
                let mut vars = std::collections::BTreeMap::new();
                vars.insert("it".to_string(), ExprVal::from_json(element));
                if let Some(user_vars) = params.get("vars").and_then(|v| v.as_object()) {
                    for (k, v) in user_vars {
                        vars.insert(k.clone(), ExprVal::from_json(v));
                    }
                }
                match eval_expr_value(formula, &vars) {
                    Ok(result) => result.truthy(),
                    Err(_) => false,
                }
            } else {
                ExprVal::from_json(element).truthy()
            }
        });

        Ok(ToolResult::ok(if satisfied { "true" } else { "false" }))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AllInput {
    items: Vec<Value>,
    #[serde(default, rename = "where")]
    where_formula: Option<String>,
    #[serde(default)]
    vars: Option<serde_json::Map<String, Value>>,
}

pub struct AllTool;

#[async_trait]
impl Tool for AllTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "all",
            "Returns \"true\" if all elements satisfy the predicate, \"false\" otherwise. With \
             `where`, evaluates the formula per element with `it` bound. Without `where`, checks if \
             all elements are truthy. Empty list → vacuously \"true\" (documented).",
            mark_flux_expr(flux_spec::tool_input_schema::<AllInput>(), &["where"]),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        use flux_lang::expr::{eval_expr_value, ExprVal};

        let items = arr_or_empty(&params, "items", "all")?;
        let where_formula = params.get("where").and_then(|v| v.as_str());

        let all_satisfy = items.iter().all(|element| {
            if let Some(formula) = where_formula {
                let mut vars = std::collections::BTreeMap::new();
                vars.insert("it".to_string(), ExprVal::from_json(element));
                if let Some(user_vars) = params.get("vars").and_then(|v| v.as_object()) {
                    for (k, v) in user_vars {
                        vars.insert(k.clone(), ExprVal::from_json(v));
                    }
                }
                match eval_expr_value(formula, &vars) {
                    Ok(result) => result.truthy(),
                    Err(_) => false,
                }
            } else {
                ExprVal::from_json(element).truthy()
            }
        });

        Ok(ToolResult::ok(if all_satisfy { "true" } else { "false" }))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct HasInput {
    items: Vec<Value>,
    value: Value,
}

pub struct HasTool;

#[async_trait]
impl Tool for HasTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "has",
            "Test if an array contains a value (JSON equality). Returns \"true\" or \"false\".",
            flux_spec::tool_input_schema::<HasInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = arr_or_empty(&params, "items", "has")?;
        let value = params
            .get("value")
            .ok_or_else(|| Error::Other("has: required param `value` missing".into()))?;

        let found = items.iter().any(|item| item == value);
        Ok(ToolResult::ok(if found { "true" } else { "false" }))
    }
}

// ---------------------------------------------------------------------------
// tests (hermetic — no filesystem, no network)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use flux_spec::Effect;
    use flux_system::{System, Workspace};

    /// Pure ops ignore the context, but `execute` still takes one. Build a throwaway.
    fn ctx() -> ToolContext {
        let dir = std::env::temp_dir().join(format!("flux-cognition-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())))
    }

    /// Every cognition spec must be pure: a real input schema, no effects, low risk, idempotent.
    #[test]
    fn specs_are_pure() {
        let mut reg = ToolRegistry::new();
        register_cognition(&mut reg);
        for spec in reg.specs() {
            assert!(
                spec.effects.is_empty(),
                "{} must declare no effects (pure)",
                spec.name
            );
            assert!(!spec.has_effect(Effect::Write));
            assert!(!spec.has_effect(Effect::Process));
            assert!(!spec.has_effect(Effect::Network));
            assert_eq!(spec.risk, Risk::Low, "{} risk", spec.name);
            assert_eq!(
                spec.idempotency,
                Idempotency::Idempotent,
                "{} idempotency",
                spec.name
            );
            assert!(spec.access.is_empty(), "{} access", spec.name);
            assert_eq!(spec.input_schema["type"], "object", "{} schema", spec.name);
            assert!(
                spec.input_schema.get("properties").is_some(),
                "{} schema has properties",
                spec.name
            );
        }
    }

    #[test]
    fn registers_all_named_ops() {
        let mut reg = ToolRegistry::new();
        register_cognition(&mut reg);
        let mut names = reg.names();
        names.sort();
        assert_eq!(
            names,
            vec![
                "all",
                "any",
                "cite",
                "coalesce",
                "compare",
                "count_by",
                "dedupe",
                "filter",
                "first",
                "flatten",
                "gaps",
                "group_by",
                "has",
                "join",
                "keys",
                "last",
                "len",
                "map",
                "merge",
                "merge_obj",
                "need",
                "omit",
                "pick",
                "regex_extract",
                "regex_match",
                "review.aggregate",
                "review.normalize",
                "skip",
                "sort",
                "split",
                "sum",
                "top",
                "values"
            ]
        );
    }

    #[tokio::test]
    async fn len_counts_arrays_and_strings() {
        let c = ctx();
        assert_eq!(
            LenTool
                .execute(&c, json!({"items": [1, 2, 3]}))
                .await
                .unwrap()
                .content,
            "3"
        );
        assert_eq!(
            LenTool
                .execute(&c, json!({"items": "hello"}))
                .await
                .unwrap()
                .content,
            "5"
        );
        let err = LenTool
            .execute(&c, json!({"items": 42}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("array or a string"), "got: {err}");
    }

    #[tokio::test]
    async fn first_and_last_pick_ends_or_null() {
        let c = ctx();
        assert_eq!(
            FirstTool
                .execute(&c, json!({"items": [1, 2, 3]}))
                .await
                .unwrap()
                .content,
            "1"
        );
        assert_eq!(
            LastTool
                .execute(&c, json!({"items": [1, 2, 3]}))
                .await
                .unwrap()
                .content,
            "3"
        );
        // Empty list yields null, not an error.
        assert_eq!(
            FirstTool
                .execute(&c, json!({"items": []}))
                .await
                .unwrap()
                .content,
            "null"
        );
        // C-235/C-236: a string element yields the raw string, not its JSON encoding — the same
        // rule `regex_extract` follows. An object element stays JSON-encoded (the re-parse rule
        // reads it back).
        assert_eq!(
            FirstTool
                .execute(&c, json!({"items": ["alpha", "beta"]}))
                .await
                .unwrap()
                .content,
            "alpha"
        );
        assert_eq!(
            LastTool
                .execute(&c, json!({"items": [{"k": 1}, {"k": 2}]}))
                .await
                .unwrap()
                .content,
            r#"{"k":2}"#
        );
    }

    #[tokio::test]
    async fn need_constructs_artifact_with_done_when_default() {
        let c = ctx();
        let r = NeedTool
            .execute(
                &c,
                json!({"ask": "ship it", "require": ["owner", "deadline"]}),
            )
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(v["ask"], "ship it");
        assert_eq!(v["require"], json!(["owner", "deadline"]));
        assert!(
            v["done_when"].is_null(),
            "absent done_when defaults to null"
        );

        // Missing `ask` is a clear error, not a panic.
        let err = NeedTool
            .execute(&c, json!({"require": []}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("ask"), "got: {err}");
    }

    #[tokio::test]
    async fn gaps_returns_uncovered_required_fields() {
        let c = ctx();
        // `owner` is covered by text mention; `budget` by a truthy field; `deadline` is unmet.
        let claims = json!([
            {"text": "the owner is Alice"},
            {"text": "an unrelated note", "budget": 1000}
        ]);
        let need = json!({"ask": "plan", "require": ["owner", "budget", "deadline"]});
        let r = GapsTool
            .execute(&c, json!({"claims": claims, "need": need}))
            .await
            .unwrap();
        let unmet: Vec<String> = serde_json::from_str(&r.content).unwrap();
        assert_eq!(unmet, vec!["deadline".to_string()]);

        // A bare-string claim mentioning the field also counts as coverage.
        let r2 = GapsTool
            .execute(
                &c,
                json!({"claims": ["deadline is friday"], "need": {"require": ["deadline"]}}),
            )
            .await
            .unwrap();
        assert_eq!(r2.content, "[]");

        // A missing `need` is a clear error.
        let err = GapsTool
            .execute(&c, json!({"claims": []}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("need"), "got: {err}");
    }

    #[tokio::test]
    async fn compare_splits_added_removed_common() {
        let c = ctx();
        let r = CompareTool
            .execute(&c, json!({"a": [1, 2, 3], "b": [2, 3, 4]}))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(v["added"], json!([4]));
        assert_eq!(v["removed"], json!([1]));
        assert_eq!(v["common"], json!([2, 3]));
    }

    #[tokio::test]
    async fn top_takes_first_n() {
        let c = ctx();
        let r = TopTool
            .execute(&c, json!({"items": [1, 2, 3, 4], "n": 2}))
            .await
            .unwrap();
        assert_eq!(r.content, "[1,2]");

        // n larger than the list returns the whole list (no panic).
        let r2 = TopTool
            .execute(&c, json!({"items": [1], "n": 5}))
            .await
            .unwrap();
        assert_eq!(r2.content, "[1]");
    }

    #[tokio::test]
    async fn merge_concatenates_in_order() {
        let c = ctx();
        let r = MergeTool
            .execute(&c, json!({"lists": [[1, 2], [], [3], [4, 5]]}))
            .await
            .unwrap();
        assert_eq!(r.content, "[1,2,3,4,5]");

        // A non-array element is a clear error.
        let err = MergeTool
            .execute(&c, json!({"lists": [[1], "nope"]}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not an array"), "got: {err}");
    }

    #[tokio::test]
    async fn cite_renders_markdown_lines() {
        let c = ctx();
        let r = CiteTool
            .execute(
                &c,
                json!({"claims": [
                    {"text": "sky is blue", "source": "wiki", "span": "p2"},
                    {"text": "no source here"},
                    "a bare string claim"
                ]}),
            )
            .await
            .unwrap();
        let expected =
            "- \"sky is blue\" (wiki: p2)\n- \"no source here\"\n- \"a bare string claim\"";
        assert_eq!(r.content, expected);
    }

    // -----------------------------------------------------------------------
    // review.normalize / review.aggregate (L-12)
    // -----------------------------------------------------------------------

    fn finding(
        severity: &str,
        category: &str,
        file: &str,
        line: i64,
        title: &str,
        confidence: f64,
        reviewer: &str,
    ) -> Value {
        json!({
            "severity": severity,
            "category": category,
            "file": file,
            "line": line,
            "title": title,
            "evidence": "some evidence",
            "recommendation": "fix it",
            "confidence": confidence,
            "reviewer": reviewer,
        })
    }

    #[tokio::test]
    async fn review_aggregate_is_stable_and_quarantines_malformed_entries() {
        let c = ctx();
        let raw = json!([
            finding("high", "security", "a.rs", 10, "sql injection", 0.9, "security"),
            finding("medium", "correctness", "b.rs", 5, "off by one", 0.7, "correctness"),
            "this is not a finding object",
            {"category": "security", "severity": "high"}, // missing title
        ]);

        let r1 = ReviewAggregateTool
            .execute(&c, json!({"findings": raw, "files": ["a.rs", "b.rs"], "reviewers": ["security", "correctness"]}))
            .await
            .unwrap();
        let r2 = ReviewAggregateTool
            .execute(&c, json!({"findings": raw, "files": ["a.rs", "b.rs"], "reviewers": ["security", "correctness"]}))
            .await
            .unwrap();

        // Byte-identical across two independent runs on the same inputs.
        assert_eq!(r1.content, r2.content, "aggregation must be deterministic");

        let report: Value = serde_json::from_str(&r1.content).unwrap();
        let findings = report["findings"].as_array().unwrap();
        assert_eq!(
            findings.len(),
            2,
            "only the 2 well-formed findings: {findings:?}"
        );

        let gaps = report["gaps"].as_array().unwrap();
        assert_eq!(
            gaps.len(),
            2,
            "the 2 malformed entries become gaps: {gaps:?}"
        );
        for g in gaps {
            let s = g.as_str().unwrap();
            assert!(
                s.starts_with("dropped malformed finding:")
                    || s.starts_with("unparseable reviewer output"),
                "gap must be human-readable, got: {s}"
            );
        }
        // Malformed entries never surface as findings.
        assert!(!findings.iter().any(|f| f.as_str().is_some()));
    }

    #[tokio::test]
    async fn review_aggregate_recovers_findings_from_dirty_reviewer_blobs() {
        // L-14: real reviewers wrap their JSON in fences or prose despite instructions. Each raw
        // `task` output arrives as ONE entry (string or parsed array); findings must be recovered —
        // not hard-fail the flow after the sub-agent spend — and unrecoverable blobs quarantine.
        let c = ctx();
        let fenced = format!(
            "```json\n{}\n```",
            json!([finding(
                "high", "security", "a.rs", 10, "sqli", 0.9, "security"
            )])
        );
        let prose = format!(
            "Here are my findings:\n\n{}\n\nLet me know if you need more.",
            json!([finding(
                "medium",
                "correctness",
                "b.rs",
                5,
                "off by one",
                0.7,
                "correctness"
            )])
        );
        let raw = json!([
            fenced,                               // fenced JSON blob
            prose,                                // prose-wrapped array
            "I found no issues worth reporting.", // unrecoverable → one gap
            [finding(
                "low",
                "maintainability",
                "c.rs",
                2,
                "long fn",
                0.5,
                "maintainability"
            )],
        ]);
        let r = ReviewAggregateTool
            .execute(
                &c,
                json!({"findings": raw, "files": ["a.rs"], "reviewers": ["security", "correctness", "maintainability"]}),
            )
            .await
            .unwrap();
        assert!(!r.is_error);
        let report: Value = serde_json::from_str(&r.content).unwrap();
        let findings = report["findings"].as_array().unwrap();
        assert_eq!(
            findings.len(),
            3,
            "fenced + prose + nested-array findings all recovered: {findings:?}"
        );
        let gaps = report["gaps"].as_array().unwrap();
        assert_eq!(gaps.len(), 1, "the junk blob quarantines: {gaps:?}");
        assert!(gaps[0]
            .as_str()
            .unwrap()
            .starts_with("unparseable reviewer output"));
    }

    #[tokio::test]
    async fn fingerprint_is_stable_and_duplicate_across_reviewers_collapses_with_agreement() {
        let c = ctx();
        // Same category+file+line+title from two different reviewers -> same fingerprint -> one
        // finding with agreement == 2.
        let raw = json!([
            finding(
                "high",
                "security",
                "a.rs",
                10,
                "SQL Injection!!",
                0.8,
                "security"
            ),
            finding(
                "high",
                "security",
                "a.rs",
                10,
                "  sql   injection  ",
                0.6,
                "correctness"
            ),
        ]);
        let r = ReviewAggregateTool
            .execute(&c, json!({"findings": raw}))
            .await
            .unwrap();
        let report: Value = serde_json::from_str(&r.content).unwrap();
        let findings = report["findings"].as_array().unwrap();
        assert_eq!(
            findings.len(),
            1,
            "duplicate fingerprints collapse: {findings:?}"
        );
        assert_eq!(findings[0]["agreement"], 2);
        // The stronger (max) confidence wins.
        assert_eq!(findings[0]["confidence"], 0.8);

        // Differing title/category/file/line all yield a DIFFERENT fingerprint.
        let a = ReviewNormalizeTool
            .execute(
                &c,
                json!({"findings": [finding("high", "security", "a.rs", 10, "x", 0.5, "r")]}),
            )
            .await
            .unwrap();
        let a_report: Value = serde_json::from_str(&a.content).unwrap();
        let fp_a = a_report["findings"][0]["fingerprint"]
            .as_str()
            .unwrap()
            .to_string();

        let variants = [
            finding("high", "correctness", "a.rs", 10, "x", 0.5, "r"), // category differs
            finding("high", "security", "b.rs", 10, "x", 0.5, "r"),    // file differs
            finding("high", "security", "a.rs", 11, "x", 0.5, "r"),    // line differs
            finding("high", "security", "a.rs", 10, "y", 0.5, "r"),    // title differs
        ];
        for v in variants {
            let r = ReviewNormalizeTool
                .execute(&c, json!({"findings": [v]}))
                .await
                .unwrap();
            let rep: Value = serde_json::from_str(&r.content).unwrap();
            let fp = rep["findings"][0]["fingerprint"].as_str().unwrap();
            assert_ne!(fp, fp_a, "a variant must yield a different fingerprint");
        }
    }

    #[tokio::test]
    async fn ranking_order_is_severity_then_confidence_then_agreement() {
        let c = ctx();
        // Tier 1: severity disambiguates (critical beats high regardless of confidence).
        // Tier 2: same severity, confidence disambiguates.
        // Tier 3: same severity+confidence, agreement disambiguates (needs a duplicate to raise it).
        let raw = json!([
            finding("high", "cat", "f.rs", 1, "high conf", 0.5, "r1"),
            finding("critical", "cat", "f.rs", 2, "critical low conf", 0.1, "r1"),
            finding("high", "cat", "f.rs", 3, "high high conf", 0.9, "r1"),
            // two entries sharing a fingerprint (same category/file/line/title) at medium severity,
            // same confidence, to produce agreement == 2 for the tier-3 case.
            finding("medium", "cat", "f.rs", 4, "agreed twice", 0.5, "r1"),
            finding("medium", "cat", "f.rs", 4, "agreed twice", 0.5, "r2"),
            finding("medium", "cat", "f.rs", 5, "agreed once", 0.5, "r3"),
        ]);
        let r = ReviewAggregateTool
            .execute(&c, json!({"findings": raw}))
            .await
            .unwrap();
        let report: Value = serde_json::from_str(&r.content).unwrap();
        let findings = report["findings"].as_array().unwrap();

        let titles: Vec<&str> = findings
            .iter()
            .map(|f| f["title"].as_str().unwrap())
            .collect();
        assert_eq!(
            titles,
            vec![
                "critical low conf", // severity wins over confidence
                "high high conf",    // same severity as next: higher confidence first
                "high conf",
                "agreed twice", // medium tier: agreement 2 beats agreement 1
                "agreed once",
            ],
            "unexpected ranking order: {titles:?}"
        );
        assert_eq!(findings[3]["agreement"], 2);
        assert_eq!(findings[4]["agreement"], 1);
    }

    // -----------------------------------------------------------------------
    // regex_match / regex_extract (L-50)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn regex_match_true_false() {
        let c = ctx();
        // Match found -> "true"
        let r = RegexMatchTool
            .execute(&c, json!({"s": "hello world", "pattern": r"w\w+d"}))
            .await
            .unwrap();
        assert_eq!(r.content, "true");

        // No match -> "false"
        let r2 = RegexMatchTool
            .execute(&c, json!({"s": "hello world", "pattern": r"xyz"}))
            .await
            .unwrap();
        assert_eq!(r2.content, "false");
    }

    #[tokio::test]
    async fn regex_match_rejects_oversize_pattern() {
        let c = ctx();
        let long_pattern = "a".repeat(513);
        let err = RegexMatchTool
            .execute(&c, json!({"s": "test", "pattern": long_pattern}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("pattern exceeds 512 chars"), "got: {err}");
    }

    #[tokio::test]
    async fn regex_match_reports_bad_pattern() {
        let c = ctx();
        let err = RegexMatchTool
            .execute(&c, json!({"s": "test", "pattern": "[unclosed"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid pattern"), "got: {err}");
    }

    #[tokio::test]
    async fn regex_extract_first_and_all() {
        let c = ctx();
        // Extract first match of group 0 (whole match) — the raw string, NOT its JSON encoding
        // (C-235/C-236): the value must be directly usable as another op's argument.
        let r = RegexExtractTool
            .execute(
                &c,
                json!({"s": "v1.2.3 and v4.5.6", "pattern": r"v\d+\.\d+\.\d+"}),
            )
            .await
            .unwrap();
        assert_eq!(r.content, "v1.2.3");

        // Extract all matches of group 0 — a structured (array) result stays JSON-encoded; the
        // runtime's string-leaf re-parse rule (C-10) reads it back.
        let r2 = RegexExtractTool
            .execute(
                &c,
                json!({"s": "v1.2.3 and v4.5.6", "pattern": r"v\d+\.\d+\.\d+", "all": true}),
            )
            .await
            .unwrap();
        let arr: Vec<String> = serde_json::from_str(&r2.content).unwrap();
        assert_eq!(arr, vec!["v1.2.3", "v4.5.6"]);

        // Extract first match of a capture group
        let r3 = RegexExtractTool
            .execute(
                &c,
                json!({"s": "version 1.2.3", "pattern": r"version (\d+\.\d+\.\d+)", "group": 1}),
            )
            .await
            .unwrap();
        assert_eq!(r3.content, "1.2.3");

        // Extract all matches of a capture group
        let r4 = RegexExtractTool
            .execute(&c, json!({"s": "v1.2.3 and v4.5.6", "pattern": r"v(\d+)\.\d+\.\d+", "group": 1, "all": true}))
            .await
            .unwrap();
        let arr2: Vec<String> = serde_json::from_str(&r4.content).unwrap();
        assert_eq!(arr2, vec!["1", "4"]);
    }

    /// C-235: the extracted string feeds an argument parser verbatim — with the old JSON-quoted
    /// form the URL parse below fails with "relative URL without a base" (the 0.36.0 smoke test).
    #[tokio::test]
    async fn regex_extract_yields_a_string_usable_as_another_ops_argument() {
        let c = ctx();
        let r = RegexExtractTool
            .execute(
                &c,
                json!({"s": "runner: http://127.0.0.1:9101 task t_1", "pattern": "runner: (\\S+)", "group": 1}),
            )
            .await
            .unwrap();
        assert!(
            !r.content.contains('"'),
            "the extracted string must not carry JSON quotes: {}",
            r.content
        );
        assert_eq!(r.content, "http://127.0.0.1:9101");
    }

    #[tokio::test]
    async fn regex_extract_null_on_no_match() {
        let c = ctx();
        let r = RegexExtractTool
            .execute(&c, json!({"s": "hello", "pattern": r"xyz"}))
            .await
            .unwrap();
        assert_eq!(r.content, "null");
    }

    #[tokio::test]
    async fn regex_extract_bad_group_errors() {
        let c = ctx();
        let err = RegexExtractTool
            .execute(&c, json!({"s": "hello", "pattern": r"h(e)llo", "group": 5}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("no capture group 5"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // pick / omit / merge_obj / coalesce / keys / values (L-49)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn pick_single_object() {
        let c = ctx();
        let r = PickTool
            .execute(
                &c,
                json!({"items": {"a": 1, "b": 2, "c": 3}, "keys": ["a", "c"]}),
            )
            .await
            .unwrap();
        let out: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(out, json!({"a": 1, "c": 3}));
    }

    #[tokio::test]
    async fn pick_over_array_of_objects() {
        let c = ctx();
        let r = PickTool
            .execute(
                &c,
                json!({"items": [{"x": 1, "y": 2}, {"x": 3, "y": 4, "z": 5}], "keys": ["x", "z"]}),
            )
            .await
            .unwrap();
        let out: Vec<Value> = serde_json::from_str(&r.content).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], json!({"x": 1}));
        assert_eq!(out[1], json!({"x": 3, "z": 5}));
    }

    #[tokio::test]
    async fn omit_removes_keys_leaves_others() {
        let c = ctx();
        let r = OmitTool
            .execute(
                &c,
                json!({"items": {"a": 1, "b": 2, "c": 3}, "keys": ["b"]}),
            )
            .await
            .unwrap();
        let out: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(out, json!({"a": 1, "c": 3}));
    }

    #[tokio::test]
    async fn merge_obj_shallow_later_wins() {
        let c = ctx();
        let r = MergeObjTool
            .execute(&c, json!({"objects": [{"a": 1, "b": 2}, {"b": 3, "c": 4}]}))
            .await
            .unwrap();
        let out: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(out, json!({"a": 1, "b": 3, "c": 4}));
    }

    #[tokio::test]
    async fn coalesce_returns_first_non_empty() {
        let c = ctx();
        let r = CoalesceTool
            .execute(&c, json!({"values": [null, "", "first"]}))
            .await
            .unwrap();
        // C-235/C-236: the raw string, not its JSON encoding.
        assert_eq!(r.content, "first");
    }

    #[tokio::test]
    async fn coalesce_keeps_zero_and_false() {
        let c = ctx();
        let r1 = CoalesceTool
            .execute(&c, json!({"values": [null, "", 0, "later"]}))
            .await
            .unwrap();
        assert_eq!(r1.content, "0");

        let r2 = CoalesceTool
            .execute(&c, json!({"values": [null, false, "later"]}))
            .await
            .unwrap();
        assert_eq!(r2.content, "false");
    }

    #[tokio::test]
    async fn keys_and_values_deterministic_order() {
        let c = ctx();
        let obj = json!({"z": 3, "a": 1, "m": 2});
        let r_keys = KeysTool
            .execute(&c, json!({"item": obj.clone()}))
            .await
            .unwrap();
        let keys: Vec<String> = serde_json::from_str(&r_keys.content).unwrap();
        assert_eq!(keys, vec!["a", "m", "z"]);

        let r_values = ValuesTool.execute(&c, json!({"item": obj})).await.unwrap();
        let values: Vec<i64> = serde_json::from_str(&r_values.content).unwrap();
        assert_eq!(values, vec![1, 2, 3]);
    }

    // -----------------------------------------------------------------------
    // sum / count_by / group_by / any / all / has (L-48)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn sum_of_numbers() {
        let c = ctx();
        let r = SumTool
            .execute(&c, json!({"items": [1, 2, 3]}))
            .await
            .unwrap();
        assert_eq!(r.content, "6");
        let r2 = SumTool.execute(&c, json!({"items": []})).await.unwrap();
        assert_eq!(r2.content, "0");
    }

    #[tokio::test]
    async fn sum_with_path_and_bad_element_errors() {
        let c = ctx();
        let r = SumTool
            .execute(
                &c,
                json!({"items": [{"price": 10}, {"price": 20}, {"price": 5}], "path": "price"}),
            )
            .await
            .unwrap();
        assert_eq!(r.content, "35");

        let err = SumTool
            .execute(&c, json!({"items": [1, 2, "nope"]}))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("element 2") && err.contains("not numeric"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn count_by_orders_count_desc_then_key() {
        let c = ctx();
        let items = json!([
            {"status": "ok"}, {"status": "fail"}, {"status": "ok"},
            {"status": "ok"}, {"status": "pending"}, {"status": "fail"}
        ]);
        let r = CountByTool
            .execute(&c, json!({"items": items, "path": "status"}))
            .await
            .unwrap();
        let out: Vec<Value> = serde_json::from_str(&r.content).unwrap();
        assert_eq!(out[0]["key"], "ok");
        assert_eq!(out[0]["count"], 3);
        assert_eq!(out[1]["key"], "fail");
        assert_eq!(out[1]["count"], 2);
        assert_eq!(out[2]["key"], "pending");
        assert_eq!(out[2]["count"], 1);
    }

    #[tokio::test]
    async fn group_by_first_seen_key_order() {
        let c = ctx();
        let items = json!([
            {"type": "bug", "id": 1}, {"type": "feature", "id": 2},
            {"type": "bug", "id": 3}, {"type": "chore", "id": 4}
        ]);
        let r = GroupByTool
            .execute(&c, json!({"items": items, "path": "type"}))
            .await
            .unwrap();
        let out: Vec<Value> = serde_json::from_str(&r.content).unwrap();
        assert_eq!(out[0]["key"], "bug");
        assert_eq!(out[0]["items"].as_array().unwrap().len(), 2);
        assert_eq!(out[1]["key"], "feature");
        assert_eq!(out[2]["key"], "chore");
    }

    #[tokio::test]
    async fn any_true_on_match_false_on_empty() {
        let c = ctx();
        let r = AnyTool
            .execute(
                &c,
                json!({"items": [{"score": 10}, {"score": 50}], "where": "it.score > 40"}),
            )
            .await
            .unwrap();
        assert_eq!(r.content, "true");
        let r2 = AnyTool
            .execute(
                &c,
                json!({"items": [{"score": 10}], "where": "it.score > 40"}),
            )
            .await
            .unwrap();
        assert_eq!(r2.content, "false");
        let r3 = AnyTool.execute(&c, json!({"items": []})).await.unwrap();
        assert_eq!(r3.content, "false");
    }

    #[tokio::test]
    async fn all_vacuously_true_on_empty_list() {
        let c = ctx();
        let r = AllTool
            .execute(
                &c,
                json!({"items": [{"ok": true}, {"ok": true}], "where": "it.ok"}),
            )
            .await
            .unwrap();
        assert_eq!(r.content, "true");
        let r2 = AllTool
            .execute(
                &c,
                json!({"items": [{"ok": true}, {"ok": false}], "where": "it.ok"}),
            )
            .await
            .unwrap();
        assert_eq!(r2.content, "false");
        let r3 = AllTool
            .execute(&c, json!({"items": [], "where": "it > 0"}))
            .await
            .unwrap();
        assert_eq!(r3.content, "true");
    }

    #[tokio::test]
    async fn has_equality_membership() {
        let c = ctx();
        let r = HasTool
            .execute(&c, json!({"items": [1, 2, 3], "value": 2}))
            .await
            .unwrap();
        assert_eq!(r.content, "true");
        let r2 = HasTool
            .execute(&c, json!({"items": [1, 2, 3], "value": 5}))
            .await
            .unwrap();
        assert_eq!(r2.content, "false");
    }

    #[tokio::test]
    async fn op_expr_builtin_conformance_matrix() {
        use flux_lang::expr::{eval_expr_value, ExprVal};

        fn normalize_content(s: &str) -> Value {
            serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
        }

        fn normalize_expr(v: ExprVal) -> Value {
            normalize_content(&v.as_text())
        }

        let c = ctx();
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("nums".into(), ExprVal::from_json(&json!([1, 2, 3])));
        vars.insert("truthy".into(), ExprVal::from_json(&json!([true, "yes"])));
        vars.insert("mixed".into(), ExprVal::from_json(&json!([true, false])));
        vars.insert("words".into(), ExprVal::from_json(&json!(["a", "b"])));

        let cases = [
            (
                SumTool
                    .execute(&c, json!({"items": [1, 2, 3]}))
                    .await
                    .unwrap()
                    .content,
                eval_expr_value("sum(nums)", &vars).unwrap(),
            ),
            (
                AnyTool
                    .execute(&c, json!({"items": [true, "yes"]}))
                    .await
                    .unwrap()
                    .content,
                eval_expr_value("any(truthy)", &vars).unwrap(),
            ),
            (
                AllTool
                    .execute(&c, json!({"items": [true, false]}))
                    .await
                    .unwrap()
                    .content,
                eval_expr_value("all(mixed)", &vars).unwrap(),
            ),
            (
                HasTool
                    .execute(&c, json!({"items": [1, 2, 3], "value": 2}))
                    .await
                    .unwrap()
                    .content,
                eval_expr_value("has(nums, 2)", &vars).unwrap(),
            ),
            (
                crate::transform::JoinTool
                    .execute(&c, json!({"items": ["a", "b"], "sep": "|"}))
                    .await
                    .unwrap()
                    .content,
                eval_expr_value("join(words, '|')", &vars).unwrap(),
            ),
            (
                crate::transform::SplitTool
                    .execute(&c, json!({"s": "a|b", "sep": "|"}))
                    .await
                    .unwrap()
                    .content,
                eval_expr_value("split('a|b', '|')", &vars).unwrap(),
            ),
            (
                FirstTool
                    .execute(&c, json!({"items": ["a", "b"]}))
                    .await
                    .unwrap()
                    .content,
                eval_expr_value("first(words)", &vars).unwrap(),
            ),
            (
                LastTool
                    .execute(&c, json!({"items": ["a", "b"]}))
                    .await
                    .unwrap()
                    .content,
                eval_expr_value("last(words)", &vars).unwrap(),
            ),
            (
                LenTool
                    .execute(&c, json!({"items": ["a", "b"]}))
                    .await
                    .unwrap()
                    .content,
                eval_expr_value("len(words)", &vars).unwrap(),
            ),
        ];

        for (op, expr) in cases {
            assert_eq!(normalize_content(&op), normalize_expr(expr));
        }
    }
}
