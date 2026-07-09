//! Pure list transform ops for the cognition pack.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use flux_core::{Error, Result};
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{Idempotency, Risk, ToolSpec};

/// Register deterministic list transform ops into the cognition group.
pub fn register_transforms(registry: &mut ToolRegistry) {
    registry.register(Arc::new(MapTool));
    registry.register(Arc::new(FilterTool));
    registry.register(Arc::new(DedupeTool));
    registry.register(Arc::new(SortTool));
    registry.register(Arc::new(FlattenTool));
    registry.register(Arc::new(SkipTool));
    registry.register(Arc::new(JoinTool));
    registry.register(Arc::new(SplitTool));
}

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

fn arr_or_empty(params: &Value, key: &str, tool: &str) -> Result<Vec<Value>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => Ok(a.clone()),
        Some(_) => Err(Error::Other(format!(
            "{tool}: param `{key}` must be an array"
        ))),
    }
}

fn str_param<'a>(params: &'a Value, key: &str, tool: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Other(format!("{tool}: required string param `{key}` missing")))
}

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

fn descend_path(v: &Value, path: &str) -> Value {
    let mut current = v.clone();
    for part in path.split('.') {
        match &current {
            Value::Object(map) => match map.get(part) {
                Some(next) => current = next.clone(),
                None => return Value::Null,
            },
            Value::String(s) => {
                if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(s) {
                    match map.get(part) {
                        Some(next) => {
                            current = next.clone();
                            continue;
                        }
                        None => return Value::Null,
                    }
                }
                return Value::Null;
            }
            _ => return Value::Null,
        }
    }
    current
}

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

fn expr_to_json(v: flux_lang::expr::ExprVal) -> Value {
    match v {
        flux_lang::expr::ExprVal::Num(n) if n.fract() == 0.0 && n.abs() < 1e15 => {
            Value::Number(serde_json::Number::from(n as i64))
        }
        flux_lang::expr::ExprVal::Num(n) => serde_json::Number::from_f64(n)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        flux_lang::expr::ExprVal::Str(s) => Value::String(s),
        flux_lang::expr::ExprVal::Bool(b) => Value::Bool(b),
        flux_lang::expr::ExprVal::List(items) => Value::Array(items),
        flux_lang::expr::ExprVal::Obj(map) => Value::Object(map),
    }
}

fn expr_vars(
    params: &Value,
    it: Option<&Value>,
    tool: &str,
) -> Result<BTreeMap<String, flux_lang::expr::ExprVal>> {
    let mut vars = BTreeMap::new();
    if let Some(it) = it {
        vars.insert("it".to_string(), flux_lang::expr::ExprVal::from_json(it));
    }
    let user_vars = match params.get("vars") {
        None | Some(Value::Null) => None,
        Some(Value::Object(map)) => Some(map),
        Some(_) => {
            return Err(Error::Other(format!(
                "{tool}: param `vars` must be an object"
            )))
        }
    };
    if let Some(user_vars) = user_vars {
        for (k, v) in user_vars {
            if k == "it" {
                return Err(Error::Other(format!("{tool}: `vars.it` is reserved")));
            }
            vars.insert(k.clone(), flux_lang::expr::ExprVal::from_json(v));
        }
    }
    Ok(vars)
}

fn validate_formula(tool: &str, formula: &str, params: &Value) -> Result<()> {
    if formula.starts_with('.') {
        return Err(Error::Other(format!(
            "{tool}: element fields are `it.<field>`, not `.<field>`"
        )));
    }
    if formula.contains('$') {
        return Err(Error::Other(format!(
            "{tool}: symbols go in `vars`, elements are `it.<field>`"
        )));
    }
    let vars = expr_vars(params, Some(&Value::Null), tool)?;
    let keys: BTreeSet<&str> = vars.keys().map(String::as_str).collect();
    let diags = flux_lang::expr::validate_expr_formula(formula, &keys);
    if let Some(first) = diags.first() {
        return Err(Error::Other(format!("{tool}: {first}")));
    }
    Ok(())
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MapInput {
    items: Vec<Value>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    expr: Option<String>,
    #[serde(default)]
    vars: Option<serde_json::Map<String, Value>>,
}

pub struct MapTool;

#[async_trait]
impl Tool for MapTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "map",
            "Project each element: `path` plucks a dotted field (missing -> `null`); `expr` evaluates \
             a formula with `it` bound to the element. Exactly one of `path` or `expr` required.",
            mark_flux_expr(flux_spec::tool_input_schema::<MapInput>(), &["expr"]),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = arr_or_empty(&params, "items", "map")?;
        let path = params.get("path").and_then(|v| v.as_str());
        let expr = params.get("expr").and_then(|v| v.as_str());
        match (path, expr) {
            (None, None) => {
                return Err(Error::Other(
                    "map: exactly one of `path` or `expr` required".into(),
                ))
            }
            (Some(_), Some(_)) => {
                return Err(Error::Other(
                    "map: `path` and `expr` are mutually exclusive".into(),
                ))
            }
            _ => {}
        }

        let out: Result<Vec<Value>> = if let Some(path) = path {
            Ok(items.iter().map(|elem| descend_path(elem, path)).collect())
        } else {
            let formula = expr.expect("checked above");
            validate_formula("map", formula, &params)?;
            items
                .iter()
                .map(|elem| {
                    let vars = expr_vars(&params, Some(elem), "map")?;
                    flux_lang::expr::eval_expr_value(formula, &vars)
                        .map(expr_to_json)
                        .map_err(|e| Error::Other(format!("map: expr evaluation failed: {e}")))
                })
                .collect()
        };
        Ok(ToolResult::ok(serde_json::to_string(&out?)?))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FilterInput {
    items: Vec<Value>,
    #[serde(default)]
    by: Option<String>,
    #[serde(default)]
    equals: Option<Value>,
    #[serde(default, rename = "where")]
    where_formula: Option<String>,
    #[serde(default)]
    vars: Option<serde_json::Map<String, Value>>,
}

pub struct FilterTool;

#[async_trait]
impl Tool for FilterTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "filter",
            "Keep array items that satisfy a predicate. With `where` (formula with `it` bound), keeps \
             elements where the formula is truthy. With `by` (dotted path), inspects that field; with \
             `equals`, matches value. `where` and `by`/`equals` are mutually exclusive.",
            mark_flux_expr(flux_spec::tool_input_schema::<FilterInput>(), &["where"]),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = arr_or_empty(&params, "items", "filter")?;
        let by = params.get("by").and_then(|v| v.as_str());
        let equals = params.get("equals");
        let where_formula = params.get("where").and_then(|v| v.as_str());

        if where_formula.is_some() && (by.is_some() || equals.is_some()) {
            return Err(Error::Other(
                "filter: `where` and `by`/`equals` are mutually exclusive".into(),
            ));
        }

        let out: Result<Vec<Value>> = if let Some(formula) = where_formula {
            validate_formula("filter", formula, &params)?;
            items
                .into_iter()
                .filter_map(|elem| {
                    let vars = match expr_vars(&params, Some(&elem), "filter") {
                        Ok(vars) => vars,
                        Err(e) => return Some(Err(e)),
                    };
                    match flux_lang::expr::eval_expr_value(formula, &vars) {
                        Ok(result) if result.truthy() => Some(Ok(elem)),
                        Ok(_) => None,
                        Err(e) => Some(Err(Error::Other(format!(
                            "filter: where evaluation failed: {e}"
                        )))),
                    }
                })
                .collect()
        } else {
            Ok(items
                .into_iter()
                .filter(|it| {
                    let probe = match by {
                        Some(path) => descend_path(it, path),
                        None => it.clone(),
                    };
                    match equals {
                        Some(eq) => &probe == eq,
                        None => flux_lang::expr::ExprVal::from_json(&probe).truthy(),
                    }
                })
                .collect())
        };

        Ok(ToolResult::ok(serde_json::to_string(&out?)?))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DedupeInput {
    items: Vec<Value>,
    #[serde(default)]
    by: Option<String>,
}

pub struct DedupeTool;

#[async_trait]
impl Tool for DedupeTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "dedupe",
            "Remove duplicates from an array, preserving first-seen order. Pass `by` (dotted path) \
             to de-duplicate on that field.",
            flux_spec::tool_input_schema::<DedupeInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = arr_or_empty(&params, "items", "dedupe")?;
        let by = params.get("by").and_then(|v| v.as_str());
        let mut out = Vec::new();
        let mut keys = Vec::new();
        for it in items {
            let key = by
                .map(|path| descend_path(&it, path))
                .unwrap_or_else(|| it.clone());
            if !keys.contains(&key) {
                keys.push(key);
                out.push(it);
            }
        }
        Ok(ToolResult::ok(serde_json::to_string(&out)?))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SortInput {
    items: Vec<Value>,
    #[serde(default)]
    by: Option<String>,
    #[serde(default)]
    order: Option<String>,
}

pub struct SortTool;

#[async_trait]
impl Tool for SortTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "sort",
            "Stably sort an array. Pass `by` (dotted path) to sort on a field, and `order` \
             (\"asc\" | \"desc\", default \"asc\").",
            flux_spec::tool_input_schema::<SortInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let mut items = arr_or_empty(&params, "items", "sort")?;
        let by = params.get("by").and_then(|v| v.as_str());
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
        let key = |v: &Value| {
            by.map(|path| descend_path(v, path))
                .unwrap_or_else(|| v.clone())
        };
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

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FlattenInput {
    items: Vec<Value>,
    #[serde(default)]
    depth: Option<u32>,
}

pub struct FlattenTool;

#[async_trait]
impl Tool for FlattenTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "flatten",
            "Flatten nested arrays `depth` levels (default 1, max 8). Non-array elements pass through; \
             string elements that are JSON arrays re-parse first.",
            flux_spec::tool_input_schema::<FlattenInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = arr_or_empty(&params, "items", "flatten")?;
        let depth = params.get("depth").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
        if depth > 8 {
            return Err(Error::Other("flatten: depth must be <= 8".into()));
        }
        fn flatten_level(items: Vec<Value>, remaining: u32) -> Vec<Value> {
            if remaining == 0 {
                return items;
            }
            let mut out = Vec::new();
            for item in items {
                match parse_json_array_string(item) {
                    Value::Array(arr) => out.extend(arr),
                    other => out.push(other),
                }
            }
            if remaining > 1 {
                flatten_level(out, remaining - 1)
            } else {
                out
            }
        }
        Ok(ToolResult::ok(serde_json::to_string(&flatten_level(
            items, depth,
        ))?))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SkipInput {
    items: Vec<Value>,
    n: i64,
}

pub struct SkipTool;

#[async_trait]
impl Tool for SkipTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "skip",
            "Drop first `n` items (mirror of `top`); `n <= 0` returns unchanged; `n >= len` returns `[]`.",
            flux_spec::tool_input_schema::<SkipInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = arr_or_empty(&params, "items", "skip")?;
        let n = params
            .get("n")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| Error::Other("skip: required integer param `n` missing".into()))?;
        let out = if n <= 0 {
            items
        } else {
            items.into_iter().skip(n as usize).collect()
        };
        Ok(ToolResult::ok(serde_json::to_string(&out)?))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct JoinInput {
    items: Vec<Value>,
    #[serde(default)]
    sep: Option<String>,
}

pub struct JoinTool;

#[async_trait]
impl Tool for JoinTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "join",
            "Stringify each element (strings as-is, others compact JSON) and join with separator \
             (default \"\\n\"). Returns plain text, not JSON.",
            flux_spec::tool_input_schema::<JoinInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let items = arr_or_empty(&params, "items", "join")?;
        let sep = params.get("sep").and_then(|v| v.as_str()).unwrap_or("\n");
        let parts: Vec<String> = items
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            })
            .collect();
        Ok(ToolResult::ok(parts.join(sep)))
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SplitInput {
    s: String,
    #[serde(default)]
    sep: Option<String>,
    #[serde(default)]
    trim: Option<bool>,
}

pub struct SplitTool;

#[async_trait]
impl Tool for SplitTool {
    fn spec(&self) -> ToolSpec {
        pure_spec(
            "split",
            "Split string on separator (default \"\\n\"). If `trim` is true, trim each part. \
             Empty input returns `[]`. Returns JSON array of strings.",
            flux_spec::tool_input_schema::<SplitInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let s = str_param(&params, "s", "split")?;
        let sep = params.get("sep").and_then(|v| v.as_str()).unwrap_or("\n");
        let trim = params
            .get("trim")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if s.is_empty() {
            return Ok(ToolResult::ok("[]".to_string()));
        }
        let parts: Vec<Value> = s
            .split(sep)
            .map(|part| Value::String(if trim { part.trim() } else { part }.to_string()))
            .collect();
        Ok(ToolResult::ok(serde_json::to_string(&parts)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_system::{System, Workspace};
    use serde_json::json;

    fn ctx() -> ToolContext {
        let dir = std::env::temp_dir().join(format!("flux-transform-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())))
    }

    #[tokio::test]
    async fn map_path_plucks_dotted_field() {
        let c = ctx();
        let r = MapTool
            .execute(
                &c,
                json!({"items": [{"author": {"name": "Ada"}}, {"author": {}}, {"author": {"name": "Lin"}}], "path": "author.name"}),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&r.content).unwrap(),
            json!(["Ada", null, "Lin"])
        );
    }

    #[tokio::test]
    async fn map_expr_evaluates_it() {
        let c = ctx();
        let r = MapTool
            .execute(
                &c,
                json!({"items": [{"score": 2}, {"score": 5}], "expr": "it.score * scale", "vars": {"scale": 10}}),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&r.content).unwrap(),
            json!([20, 50])
        );
    }

    #[tokio::test]
    async fn filter_where_keeps_matching_predicate() {
        let c = ctx();
        let r = FilterTool
            .execute(
                &c,
                json!({"items": [{"state": "opened", "n": 3}, {"state": "closed", "n": 9}, {"state": "opened", "n": 1}], "where": "it.state == 'opened' && it.n > min", "vars": {"min": 2}}),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&r.content).unwrap(),
            json!([{"state": "opened", "n": 3}])
        );
    }

    #[tokio::test]
    async fn filter_where_with_list_builtin_predicate_runs() {
        // Regression: a predicate applying a list builtin (`has`/`any`/`sum`/…) to a dotted field
        // must not be rejected as "malformed" by the pre-run formula validation — the validator
        // cannot know `it.labels` is a list, and the op evaluates it per element with real data.
        let c = ctx();
        let r = FilterTool
            .execute(
                &c,
                json!({
                    "items": [
                        {"id": 1, "labels": ["bug", "ui"]},
                        {"id": 2, "labels": ["docs"]},
                        {"id": 3, "labels": ["backend", "bug"]}
                    ],
                    "where": "has(it.labels, 'bug')"
                }),
            )
            .await
            .unwrap();
        let kept: Vec<Value> = serde_json::from_str(&r.content).unwrap();
        assert_eq!(
            kept.iter()
                .map(|v| v["id"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 3],
            "the list-builtin predicate must select items 1 and 3"
        );
    }

    #[tokio::test]
    async fn map_expr_with_list_builtin_runs() {
        // Same regression on the `map` `expr` path: `sum(it.scores)` over a list field must run.
        let c = ctx();
        let r = MapTool
            .execute(
                &c,
                json!({
                    "items": [{"scores": [1, 2, 3]}, {"scores": [10]}],
                    "expr": "sum(it.scores)"
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&r.content).unwrap(),
            json!([6, 10])
        );
    }

    #[tokio::test]
    async fn filter_where_and_by_mutually_exclusive() {
        let c = ctx();
        let err = FilterTool
            .execute(&c, json!({"items": [], "where": "it.ok", "by": "ok"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("mutually exclusive"), "got: {err}");
    }

    #[tokio::test]
    async fn sort_and_dedupe_by_accept_dotted_paths() {
        let c = ctx();
        let items = json!([
            {"author": {"name": "Lin"}, "id": 1},
            {"author": {"name": "Ada"}, "id": 2},
            {"author": {"name": "Lin"}, "id": 3}
        ]);
        let sorted = SortTool
            .execute(&c, json!({"items": items.clone(), "by": "author.name"}))
            .await
            .unwrap();
        let sorted: Vec<Value> = serde_json::from_str(&sorted.content).unwrap();
        assert_eq!(
            sorted
                .iter()
                .map(|v| v["id"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![2, 1, 3]
        );

        let deduped = DedupeTool
            .execute(&c, json!({"items": items, "by": "author.name"}))
            .await
            .unwrap();
        let deduped: Vec<Value> = serde_json::from_str(&deduped.content).unwrap();
        assert_eq!(
            deduped
                .iter()
                .map(|v| v["id"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[tokio::test]
    async fn flatten_depth_one_and_two() {
        let c = ctx();
        let items = json!([[1], "[2,3]", [[4]], 5]);
        let one = FlattenTool
            .execute(&c, json!({"items": items.clone()}))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&one.content).unwrap(),
            json!([1, 2, 3, [4], 5])
        );

        let two = FlattenTool
            .execute(&c, json!({"items": items, "depth": 2}))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&two.content).unwrap(),
            json!([1, 2, 3, 4, 5])
        );
    }

    #[tokio::test]
    async fn skip_drops_first_n() {
        let c = ctx();
        assert_eq!(
            SkipTool
                .execute(&c, json!({"items": [1, 2, 3], "n": 2}))
                .await
                .unwrap()
                .content,
            "[3]"
        );
        assert_eq!(
            SkipTool
                .execute(&c, json!({"items": [1, 2], "n": 9}))
                .await
                .unwrap()
                .content,
            "[]"
        );
        assert_eq!(
            SkipTool
                .execute(&c, json!({"items": [1, 2], "n": 0}))
                .await
                .unwrap()
                .content,
            "[1,2]"
        );
    }

    #[tokio::test]
    async fn join_stringifies_and_joins() {
        let c = ctx();
        let r = JoinTool
            .execute(&c, json!({"items": ["a", {"b": 2}, 3], "sep": "|"}))
            .await
            .unwrap();
        assert_eq!(r.content, r#"a|{"b":2}|3"#);
    }

    #[tokio::test]
    async fn split_returns_json_array() {
        let c = ctx();
        let r = SplitTool
            .execute(&c, json!({"s": "a, b, c", "sep": ",", "trim": true}))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&r.content).unwrap(),
            json!(["a", "b", "c"])
        );
        assert_eq!(
            SplitTool
                .execute(&c, json!({"s": ""}))
                .await
                .unwrap()
                .content,
            "[]"
        );
    }

    #[tokio::test]
    async fn where_hint_on_leading_dot_and_dollar() {
        let c = ctx();
        let leading_dot = FilterTool
            .execute(&c, json!({"items": [], "where": ".state == 'open'"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(leading_dot.contains("it.<field>"), "got: {leading_dot}");

        let dollar = FilterTool
            .execute(&c, json!({"items": [], "where": "$state == 'open'"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(dollar.contains("symbols go in `vars`"), "got: {dollar}");
    }
}
