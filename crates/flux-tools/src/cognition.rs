//! `cognition` — a pack of **pure** reasoning ops (no IO).
//!
//! These tools never touch the filesystem, network, or a process — they only transform their JSON
//! arguments and hand back a JSON string. Each declares an empty effect set (`effects: vec![]`),
//! `Risk::Low`, and `Idempotency::Idempotent`, so the runtime's policy/approval gates never fire for
//! them. They give the model a small, deterministic toolbox for shaping evidence: declaring what a
//! task `need`s, finding the `gaps` against gathered claims, and `compare`/`dedupe`/`sort`/`top`/
//! `merge`/`cite`/`len`/`first`/`last`/`filter` over lists of values.
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
pub fn register_cognition(registry: &mut ToolRegistry) {
    registry.register(Arc::new(NeedTool));
    registry.register(Arc::new(GapsTool));
    registry.register(Arc::new(CompareTool));
    registry.register(Arc::new(DedupeTool));
    registry.register(Arc::new(SortTool));
    registry.register(Arc::new(TopTool));
    registry.register(Arc::new(MergeTool));
    registry.register(Arc::new(CiteTool));
    registry.register(Arc::new(LenTool));
    registry.register(Arc::new(FirstTool));
    registry.register(Arc::new(LastTool));
    registry.register(Arc::new(FilterTool));
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

/// A total order over JSON values: numbers numerically, strings/bools naturally, otherwise by a
/// stable type rank (null < bool < number < string < array < object), with arrays/objects compared
/// by their compact JSON. Never panics (NaN can't arise from JSON; it degrades to `Equal`).
fn cmp_value(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    fn rank(v: &Value) -> u8 {
        match v {
            Value::Null => 0,
            Value::Bool(_) => 1,
            Value::Number(_) => 2,
            Value::String(_) => 3,
            Value::Array(_) => 4,
            Value::Object(_) => 5,
        }
    }
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Number(x), Value::Number(y)) => x
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&y.as_f64().unwrap_or(0.0))
            .unwrap_or(Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        _ => {
            let (ra, rb) = (rank(a), rank(b));
            if ra == rb {
                a.to_string().cmp(&b.to_string())
            } else {
                ra.cmp(&rb)
            }
        }
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
// dedupe
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DedupeInput {
    items: Vec<Value>,
    /// Optional object field to de-duplicate by
    #[serde(default)]
    by: Option<String>,
}

pub struct DedupeTool;

#[async_trait]
impl Tool for DedupeTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "dedupe",
            "Remove duplicates from an array, preserving first-seen order. By default duplicates are \
             by whole-value equality; pass `by` to de-duplicate on that object field instead.",
            flux_spec::tool_input_schema::<DedupeInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = arr_or_empty(&params, "items", "dedupe")?;
        let by = match params.get("by") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err(Error::Other("dedupe: param `by` must be a string".into())),
        };
        let mut out: Vec<Value> = Vec::new();
        let mut keys: Vec<Value> = Vec::new();
        for it in items {
            let key = match &by {
                Some(f) => it.get(f.as_str()).cloned().unwrap_or(Value::Null),
                None => it.clone(),
            };
            if !keys.contains(&key) {
                keys.push(key);
                out.push(it);
            }
        }
        Ok(ToolResult::ok(serde_json::to_string(&out)?))
    }
}

// ---------------------------------------------------------------------------
// sort
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum SortOrder {
    Asc,
    Desc,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SortInput {
    items: Vec<Value>,
    /// Optional object field to sort by
    #[serde(default)]
    by: Option<String>,
    /// Sort direction (default asc)
    #[serde(default)]
    order: Option<SortOrder>,
}

pub struct SortTool;

#[async_trait]
impl Tool for SortTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "sort",
            "Stably sort an array. By default values sort naturally; pass `by` to sort on an object \
             field, and `order` (\"asc\" | \"desc\", default \"asc\") to choose direction.",
            flux_spec::tool_input_schema::<SortInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let mut items = arr_or_empty(&params, "items", "sort")?;
        let by = match params.get("by") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err(Error::Other("sort: param `by` must be a string".into())),
        };
        let desc = match params.get("order") {
            None | Some(Value::Null) => false,
            Some(Value::String(s)) if s == "asc" => false,
            Some(Value::String(s)) if s == "desc" => true,
            Some(_) => {
                return Err(Error::Other(
                    "sort: param `order` must be \"asc\" or \"desc\"".into(),
                ))
            }
        };
        let key = |v: &Value| -> Value {
            match &by {
                Some(f) => v.get(f.as_str()).cloned().unwrap_or(Value::Null),
                None => v.clone(),
            }
        };
        // `sort_by` is stable, so equal keys keep their input order.
        items.sort_by(|a, b| {
            let ord = cmp_value(&key(a), &key(b));
            if desc {
                ord.reverse()
            } else {
                ord
            }
        });
        Ok(ToolResult::ok(serde_json::to_string(&items)?))
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
            match l {
                Value::Array(a) => out.extend(a),
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
// filter
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FilterInput {
    items: Vec<Value>,
    /// Optional object field to inspect
    #[serde(default)]
    by: Option<String>,
    /// Optional value to match (default: keep truthy)
    #[serde(default)]
    equals: Option<Value>,
}

pub struct FilterTool;

#[async_trait]
impl Tool for FilterTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "filter",
            "Keep array items that satisfy a predicate. With `by`, the object field of that name is \
             inspected (otherwise the item itself). With `equals`, an item is kept when the inspected \
             value equals it; without `equals`, when the inspected value is truthy.",
            flux_spec::tool_input_schema::<FilterInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = arr_or_empty(&params, "items", "filter")?;
        let by = match params.get("by") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err(Error::Other("filter: param `by` must be a string".into())),
        };
        let equals = match params.get("equals") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.clone()),
        };
        let out: Vec<Value> = items
            .into_iter()
            .filter(|it| {
                let probe = match &by {
                    Some(f) => it.get(f.as_str()).cloned().unwrap_or(Value::Null),
                    None => it.clone(),
                };
                match &equals {
                    Some(eq) => &probe == eq,
                    None => is_truthy(&probe),
                }
            })
            .collect();
        Ok(ToolResult::ok(serde_json::to_string(&out)?))
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
                "cite", "compare", "dedupe", "filter", "first", "gaps", "last", "len", "merge",
                "need", "sort", "top"
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
    }

    #[tokio::test]
    async fn filter_by_truthy_and_by_equals() {
        let c = ctx();
        // Keep items whose `active` field is truthy.
        let r = FilterTool
            .execute(
                &c,
                json!({"items": [{"id": 1, "active": true}, {"id": 2, "active": false}], "by": "active"}),
            )
            .await
            .unwrap();
        let out: Vec<Value> = serde_json::from_str(&r.content).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], 1);

        // Keep items whose `status` field equals "ok".
        let r2 = FilterTool
            .execute(
                &c,
                json!({"items": [{"status": "ok"}, {"status": "fail"}], "by": "status", "equals": "ok"}),
            )
            .await
            .unwrap();
        let out2: Vec<Value> = serde_json::from_str(&r2.content).unwrap();
        assert_eq!(out2, json!([{"status": "ok"}]).as_array().unwrap().clone());

        // Bare truthy filter over scalars.
        let r3 = FilterTool
            .execute(&c, json!({"items": [0, 1, "", "x", false, true]}))
            .await
            .unwrap();
        assert_eq!(r3.content, "[1,\"x\",true]");
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
    async fn dedupe_whole_value_and_by_field() {
        let c = ctx();
        let r = DedupeTool
            .execute(&c, json!({"items": [1, 1, 2, 1, 3, 2]}))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<Value>>(&r.content).unwrap(),
            json!([1, 2, 3]).as_array().unwrap().clone()
        );

        let r2 = DedupeTool
            .execute(
                &c,
                json!({"items": [{"id": 1, "n": "a"}, {"id": 1, "n": "b"}, {"id": 2}], "by": "id"}),
            )
            .await
            .unwrap();
        let out: Vec<Value> = serde_json::from_str(&r2.content).unwrap();
        assert_eq!(out.len(), 2, "first-seen per `id` kept: {out:?}");
        assert_eq!(out[0]["n"], "a");
    }

    #[tokio::test]
    async fn sort_natural_by_field_and_desc() {
        let c = ctx();
        let r = SortTool
            .execute(&c, json!({"items": [3, 1, 2]}))
            .await
            .unwrap();
        assert_eq!(r.content, "[1,2,3]");

        let r2 = SortTool
            .execute(
                &c,
                json!({"items": [{"k": 2}, {"k": 1}, {"k": 3}], "by": "k", "order": "desc"}),
            )
            .await
            .unwrap();
        let out: Vec<Value> = serde_json::from_str(&r2.content).unwrap();
        assert_eq!(
            out.iter()
                .map(|v| v["k"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![3, 2, 1]
        );

        // A bad `order` is a clear error.
        let err = SortTool
            .execute(&c, json!({"items": [], "order": "sideways"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("order"), "got: {err}");
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
}
