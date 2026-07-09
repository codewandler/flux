//! Plan-delta materialization (KF3/L-55): a small JSON patch against the previous turn's
//! `DraftAst`, applied to a clone and re-decoded so it can flow through the exact same gates a
//! full `emit_plan` does (model-ingress normalization → hidden-ops surfacing → analyzer/lower).
//! `compile.rs`'s planner loop is the only caller — the runtime never sees a partial plan, only
//! the materialized, fully analyzed [`DraftAst`] [`apply_delta`] produces.
//!
//! The patch operates on the AST's JSON wire form (the exact shape `emit_plan`'s `ast` parameter
//! carries) rather than walking the typed [`Node`] enum variant-by-variant. That is what makes ONE
//! generic path walker ([`resolve_container_mut`]) cover every nested node list the analyzer can
//! name in a diagnostic path (`body[3].then[1]`, `body[0].branches[1].body[2]`, …, see
//! `flux_lang::analyze`'s `Diags::with`) — those segments are just JSON object field names one
//! level down from whatever the previous segment resolved to, with no per-`Node`-variant case
//! analysis needed.

use sha2::{Digest, Sha256};

use crate::ast::DraftAst;

/// The only delta wire version understood today. Future-proofing per the design: an incompatible
/// patch grammar bumps this rather than silently reinterpreting an old delta under new rules.
pub const DELTA_VERSION: u32 = 1;

/// A versioned patch against a previously-rejected [`DraftAst`] — the decoded `emit_plan_delta`
/// tool input. `base` pins the delta to the EXACT AST it patches (see [`ast_content_hash`]), so a
/// stale delta (the model patching an AST that is no longer "the previous one" this turn) is
/// repair feedback, never silently applied to the wrong base.
#[derive(Debug, Clone, PartialEq)]
pub struct Delta {
    pub version: u32,
    pub base: String,
    pub ops: Vec<DeltaOp>,
}

/// One patch operation within a [`Delta`], applied in order.
#[derive(Debug, Clone, PartialEq)]
pub struct DeltaOp {
    pub action: DeltaAction,
    /// The node path this op targets, in the SAME vocabulary the analyzer renders into a
    /// diagnostic's message (`body[3]`, `body[3].then[1]`, …) — always ending in an indexed
    /// segment (`name[i]`), since every action targets a position in a node list.
    pub path: String,
    /// The replacement/inserted node, as JSON (the wire form). Required for `replace`/`insert`;
    /// absent for `delete`.
    pub node: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaAction {
    /// Overwrite the node at `path`'s index.
    Replace,
    /// Insert `node` before `path`'s index (index == the list's length appends).
    Insert,
    /// Remove the node at `path`'s index.
    Delete,
}

/// A stable content hash of a [`DraftAst`]'s canonical JSON — the `base` a plan-delta pins itself
/// to. `serde_json::to_string` is deterministic here: every AST collection that could otherwise
/// reorder is either a `Vec` (declaration order preserved) or a `BTreeMap` (`Node::Obj`'s
/// `fields`, `Node::Expr`'s `vars` — sorted lexicographically), never a `HashMap`, so semantically
/// identical ASTs always hash identically regardless of how they were built.
pub fn ast_content_hash(ast: &DraftAst) -> String {
    let canonical = serde_json::to_string(ast).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(canonical.as_bytes());
    format!("{:x}", h.finalize())
}

/// Parse the raw `emit_plan_delta` tool input into a [`Delta`]. Tolerant like the rest of
/// `compile.rs`'s model-ingress parsers: a missing or malformed field is a `String` error meant to
/// ride straight back as repair feedback, never a panic.
pub fn parse_delta(input: &serde_json::Value) -> Result<Delta, String> {
    let version = input
        .get("version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "emit_plan_delta: missing/invalid `version` (must be 1)".to_string())?;
    if version != u64::from(DELTA_VERSION) {
        return Err(format!(
            "emit_plan_delta: unsupported delta version {version} (expected {DELTA_VERSION})"
        ));
    }
    let base = input
        .get("base")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            "emit_plan_delta: missing `base` — pass the previous plan's content hash from the \
             rejection feedback"
                .to_string()
        })?;
    let ops_val = input
        .get("ops")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or_else(|| {
            "emit_plan_delta: missing/empty `ops` — a delta must patch at least one node"
                .to_string()
        })?;
    let mut ops = Vec::with_capacity(ops_val.len());
    for (i, raw) in ops_val.iter().enumerate() {
        let action = match raw.get("action").and_then(|v| v.as_str()) {
            Some("replace") => DeltaAction::Replace,
            Some("insert") => DeltaAction::Insert,
            Some("delete") => DeltaAction::Delete,
            Some(other) => {
                return Err(format!(
                    "emit_plan_delta: op {i} has unknown action `{other}` — use `replace`, \
                     `insert`, or `delete`"
                ))
            }
            None => return Err(format!("emit_plan_delta: op {i} is missing `action`")),
        };
        let path = raw
            .get("path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("emit_plan_delta: op {i} is missing a non-empty `path`"))?;
        let node = raw.get("node").cloned();
        if matches!(action, DeltaAction::Replace | DeltaAction::Insert) && node.is_none() {
            return Err(format!(
                "emit_plan_delta: op {i} ({action:?} at `{path}`) requires a `node`"
            ));
        }
        ops.push(DeltaOp { action, path, node });
    }
    Ok(Delta {
        version: version as u32,
        base,
        ops,
    })
}

/// Materialize `delta` against `base`: verify the base hash, apply every op in order on a JSON
/// CLONE of `base` (never touches `base` itself), then decode the result back into a `DraftAst`.
/// A stale `base` hash, a malformed path, an out-of-range index, or a `node` payload that fails to
/// decode as a `Node` are all `Err(String)` — repair feedback for the same loop that handles a
/// rejected full plan; the previous accepted/rejected state is never mutated on any of these
/// paths.
pub fn apply_delta(base: &DraftAst, delta: &Delta) -> Result<DraftAst, String> {
    let actual = ast_content_hash(base);
    if actual != delta.base {
        return Err(format!(
            "emit_plan_delta: stale `base` (`{}`) — the plan you're patching is no longer the \
             current one (its content hash is now `{actual}`); re-emit a fresh emit_plan_delta \
             against the latest rejection, or call emit_plan with a full plan instead.",
            delta.base
        ));
    }
    let mut working = serde_json::to_value(base)
        .map_err(|e| format!("emit_plan_delta: could not serialize the base plan: {e}"))?;
    for (i, op) in delta.ops.iter().enumerate() {
        apply_op(&mut working, op).map_err(|e| {
            format!(
                "emit_plan_delta: op {i} ({:?} at `{}`): {e}",
                op.action, op.path
            )
        })?;
    }
    serde_json::from_value(working).map_err(|e| {
        format!("emit_plan_delta: the patched plan did not decode as a valid AST: {e}")
    })
}

fn apply_op(root: &mut serde_json::Value, op: &DeltaOp) -> Result<(), String> {
    match op.action {
        DeltaAction::Replace => {
            let node = op.node.clone().expect("validated by parse_delta");
            let (arr, idx) = resolve_container_mut(root, &op.path)?;
            if idx >= arr.len() {
                return Err(format!(
                    "index {idx} out of range ({} node(s) at this path)",
                    arr.len()
                ));
            }
            arr[idx] = node;
        }
        DeltaAction::Insert => {
            let node = op.node.clone().expect("validated by parse_delta");
            let (arr, idx) = resolve_container_mut(root, &op.path)?;
            if idx > arr.len() {
                return Err(format!(
                    "index {idx} out of range for insert (at most {} — {} node(s) at this path)",
                    arr.len(),
                    arr.len()
                ));
            }
            arr.insert(idx, node);
        }
        DeltaAction::Delete => {
            let (arr, idx) = resolve_container_mut(root, &op.path)?;
            if idx >= arr.len() {
                return Err(format!(
                    "index {idx} out of range ({} node(s) at this path)",
                    arr.len()
                ));
            }
            arr.remove(idx);
        }
    }
    Ok(())
}

/// Split one dot-separated path segment (`"body[3]"`, `"then[1]"`) into its field name and index.
fn split_segment(seg: &str) -> Result<(&str, usize), String> {
    let open = seg
        .find('[')
        .ok_or_else(|| format!("path segment `{seg}` is missing an index, e.g. `{seg}[0]`"))?;
    if !seg.ends_with(']') {
        return Err(format!("malformed path segment `{seg}`"));
    }
    let name = &seg[..open];
    if name.is_empty() {
        return Err(format!("path segment `{seg}` is missing a field name"));
    }
    let idx: usize = seg[open + 1..seg.len() - 1]
        .parse()
        .map_err(|_| format!("malformed index in path segment `{seg}`"))?;
    Ok((name, idx))
}

/// Walk `path` against `root` (an AST's JSON form) and return the node LIST the final segment
/// names, plus its target index — the `(array, index)` pair every op applies to. Handles arbitrary
/// nesting depth generically (see the module doc): each dot-separated segment beyond the last is
/// resolved fully (field, then index into it) to descend one node deeper; the final segment
/// resolves only its field (the array itself is returned, unindexed, so callers can replace/
/// insert/delete at `index`).
fn resolve_container_mut<'a>(
    root: &'a mut serde_json::Value,
    path: &str,
) -> Result<(&'a mut Vec<serde_json::Value>, usize), String> {
    let segs: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    let (last, init) = segs
        .split_last()
        .ok_or_else(|| "path is empty".to_string())?;
    let mut cur = root;
    for seg in init {
        let (name, idx) = split_segment(seg)?;
        cur = cur
            .get_mut(name)
            .ok_or_else(|| format!("no field `{name}` at path segment `{seg}`"))?;
        cur = cur
            .as_array_mut()
            .ok_or_else(|| format!("`{name}` is not a node list at path segment `{seg}`"))?
            .get_mut(idx)
            .ok_or_else(|| format!("index {idx} out of range at path segment `{seg}`"))?;
    }
    let (name, idx) = split_segment(last)?;
    let arr = cur
        .get_mut(name)
        .ok_or_else(|| format!("no field `{name}` at path segment `{last}`"))?
        .as_array_mut()
        .ok_or_else(|| format!("`{name}` is not a node list at path segment `{last}`"))?;
    Ok((arr, idx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ast(body_json: serde_json::Value) -> DraftAst {
        serde_json::from_value(json!({ "body": body_json })).unwrap()
    }

    #[test]
    fn hash_is_stable_and_content_sensitive() {
        let a = ast(json!([{"kind": "call", "op": "read", "args": []}]));
        let b = ast(json!([{"kind": "call", "op": "read", "args": []}]));
        let c = ast(json!([{"kind": "call", "op": "write", "args": []}]));
        assert_eq!(
            ast_content_hash(&a),
            ast_content_hash(&b),
            "same content, same hash"
        );
        assert_ne!(
            ast_content_hash(&a),
            ast_content_hash(&c),
            "different content, different hash"
        );
    }

    #[test]
    fn replace_at_top_level_swaps_the_node() {
        let base = ast(json!([
            {"kind": "call", "op": "nope", "args": []},
            {"kind": "call", "op": "read", "args": []},
        ]));
        let delta = Delta {
            version: 1,
            base: ast_content_hash(&base),
            ops: vec![DeltaOp {
                action: DeltaAction::Replace,
                path: "body[0]".to_string(),
                node: Some(json!({"kind": "call", "op": "read", "args": []})),
            }],
        };
        let out = apply_delta(&base, &delta).unwrap();
        assert_eq!(out.body.len(), 2);
        assert!(matches!(&out.body[0], crate::ast::Node::Call { op, .. } if op == "read"));
        assert!(matches!(&out.body[1], crate::ast::Node::Call { op, .. } if op == "read"));
    }

    #[test]
    fn insert_shifts_later_nodes_and_append_at_len_works() {
        let base = ast(json!([{"kind": "call", "op": "a", "args": []}]));
        let delta = Delta {
            version: 1,
            base: ast_content_hash(&base),
            ops: vec![
                DeltaOp {
                    action: DeltaAction::Insert,
                    path: "body[0]".to_string(),
                    node: Some(json!({"kind": "call", "op": "b", "args": []})),
                },
                DeltaOp {
                    action: DeltaAction::Insert,
                    path: "body[2]".to_string(), // len is now 2 → appends
                    node: Some(json!({"kind": "call", "op": "c", "args": []})),
                },
            ],
        };
        let out = apply_delta(&base, &delta).unwrap();
        let ops: Vec<&str> = out
            .body
            .iter()
            .map(|n| match n {
                crate::ast::Node::Call { op, .. } => op.as_str(),
                _ => panic!("expected call nodes"),
            })
            .collect();
        assert_eq!(ops, vec!["b", "a", "c"]);
    }

    #[test]
    fn delete_removes_exactly_one_node() {
        let base = ast(json!([
            {"kind": "call", "op": "a", "args": []},
            {"kind": "call", "op": "b", "args": []},
        ]));
        let delta = Delta {
            version: 1,
            base: ast_content_hash(&base),
            ops: vec![DeltaOp {
                action: DeltaAction::Delete,
                path: "body[0]".to_string(),
                node: None,
            }],
        };
        let out = apply_delta(&base, &delta).unwrap();
        assert_eq!(out.body.len(), 1);
        assert!(matches!(&out.body[0], crate::ast::Node::Call { op, .. } if op == "b"));
    }

    /// Full nested-path coverage: `body[i].then[i]`, one level under a `when` node — the exact
    /// shape the analyzer renders (`body[3].then[1]`).
    #[test]
    fn nested_path_reaches_into_a_when_branch() {
        let base = ast(json!([{
            "kind": "when",
            "cond": {"kind": "lit", "value": true},
            "then": [{"kind": "call", "op": "old", "args": []}],
            "otherwise": [],
        }]));
        let delta = Delta {
            version: 1,
            base: ast_content_hash(&base),
            ops: vec![DeltaOp {
                action: DeltaAction::Replace,
                path: "body[0].then[0]".to_string(),
                node: Some(json!({"kind": "call", "op": "new", "args": []})),
            }],
        };
        let out = apply_delta(&base, &delta).unwrap();
        match &out.body[0] {
            crate::ast::Node::When { then, .. } => {
                assert!(matches!(&then[0], crate::ast::Node::Call { op, .. } if op == "new"));
            }
            other => panic!("expected a when node, got {other:?}"),
        }
    }

    /// Deeper nesting than the story's "one level" floor: `branches[j].body[i]` inside a
    /// `parallel` node — proves the generic JSON walker covers arbitrary depth, not just one hop.
    #[test]
    fn deeply_nested_path_reaches_into_a_parallel_branch() {
        let base = ast(json!([{
            "kind": "parallel",
            "branches": [
                {"name": "a", "body": [{"kind": "call", "op": "old", "args": []}]},
            ],
        }]));
        let delta = Delta {
            version: 1,
            base: ast_content_hash(&base),
            ops: vec![DeltaOp {
                action: DeltaAction::Replace,
                path: "body[0].branches[0].body[0]".to_string(),
                node: Some(json!({"kind": "call", "op": "new", "args": []})),
            }],
        };
        let out = apply_delta(&base, &delta).unwrap();
        match &out.body[0] {
            crate::ast::Node::Parallel { branches } => {
                assert!(
                    matches!(&branches[0].body[0], crate::ast::Node::Call { op, .. } if op == "new")
                );
            }
            other => panic!("expected a parallel node, got {other:?}"),
        }
    }

    #[test]
    fn stale_base_is_rejected_and_base_is_untouched() {
        let base = ast(json!([{"kind": "call", "op": "a", "args": []}]));
        let delta = Delta {
            version: 1,
            base: "not-the-real-hash".to_string(),
            ops: vec![DeltaOp {
                action: DeltaAction::Delete,
                path: "body[0]".to_string(),
                node: None,
            }],
        };
        let err = apply_delta(&base, &delta).unwrap_err();
        assert!(err.contains("stale"), "{err}");
        // `base` itself was never touched — re-hash it and confirm it is unchanged.
        assert_eq!(
            ast_content_hash(&base),
            ast_content_hash(&ast(json!([{"kind": "call", "op": "a", "args": []}])))
        );
    }

    #[test]
    fn out_of_range_index_is_rejected() {
        let base = ast(json!([{"kind": "call", "op": "a", "args": []}]));
        let delta = Delta {
            version: 1,
            base: ast_content_hash(&base),
            ops: vec![DeltaOp {
                action: DeltaAction::Replace,
                path: "body[5]".to_string(),
                node: Some(json!({"kind": "call", "op": "b", "args": []})),
            }],
        };
        let err = apply_delta(&base, &delta).unwrap_err();
        assert!(err.contains("out of range"), "{err}");
    }

    #[test]
    fn a_path_into_a_non_list_field_is_rejected() {
        let base = ast(json!([{
            "kind": "when",
            "cond": {"kind": "lit", "value": true},
            "then": [],
            "otherwise": [],
        }]));
        let delta = Delta {
            version: 1,
            base: ast_content_hash(&base),
            ops: vec![DeltaOp {
                action: DeltaAction::Replace,
                // `cond` is a single node, not a list — cannot be indexed.
                path: "body[0].cond[0]".to_string(),
                node: Some(json!({"kind": "lit", "value": false})),
            }],
        };
        let err = apply_delta(&base, &delta).unwrap_err();
        assert!(err.contains("not a node list"), "{err}");
    }

    #[test]
    fn a_node_that_fails_to_decode_is_rejected() {
        let base = ast(json!([{"kind": "call", "op": "a", "args": []}]));
        let delta = Delta {
            version: 1,
            base: ast_content_hash(&base),
            ops: vec![DeltaOp {
                action: DeltaAction::Replace,
                path: "body[0]".to_string(),
                // `frobnicate` is not a real node kind.
                node: Some(json!({"kind": "frobnicate"})),
            }],
        };
        let err = apply_delta(&base, &delta).unwrap_err();
        assert!(err.contains("did not decode"), "{err}");
    }

    #[test]
    fn parse_delta_rejects_wrong_version_missing_base_and_empty_ops() {
        assert!(parse_delta(
            &json!({"version": 2, "base": "x", "ops": [{"action": "delete", "path": "body[0]"}]})
        )
        .is_err());
        assert!(parse_delta(
            &json!({"version": 1, "ops": [{"action": "delete", "path": "body[0]"}]})
        )
        .is_err());
        assert!(parse_delta(&json!({"version": 1, "base": "x", "ops": []})).is_err());
        assert!(parse_delta(
            &json!({"version": 1, "base": "x", "ops": [{"action": "bogus", "path": "body[0]"}]})
        )
        .is_err());
        assert!(parse_delta(&json!({"version": 1, "base": "x", "ops": [{"action": "replace", "path": "body[0]"}]})).is_err(), "replace without node");
        let ok = parse_delta(&json!({
            "version": 1,
            "base": "x",
            "ops": [{"action": "delete", "path": "body[0]"}],
        }))
        .unwrap();
        assert_eq!(ok.ops.len(), 1);
        assert_eq!(ok.ops[0].action, DeltaAction::Delete);
    }
}
