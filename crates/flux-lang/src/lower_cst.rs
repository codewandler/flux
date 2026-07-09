//! `lower_cst` — the CST front-end host (L-59): strict lowering from the lossless rowan tree to
//! the semantic [`DraftAst`], plus the **range side-map** that gives the message-only analyzer
//! diagnostics real [`TextRange`]s.
//!
//! # Architecture
//! The tolerant CST models statement *structure* completely, but statement headers are token runs
//! (that is what an editor needs). Reproducing the `DraftAst` — and the pinned parse-error texts —
//! from the tree alone would mean re-implementing the whole content grammar, so the lowering keeps
//! the proven line machinery in [`crate::parse`] as the **semantic authority**: behavior is
//! byte-identical by construction. What the CST contributes here:
//!
//! - **Strictness with spans** ([`cst_to_draft`]): any parser/lexer error is reported with its
//!   `TextRange` (the LSP path), and a clean tree lowers to the exact legacy `DraftAst`.
//! - **The range side-map** ([`RangeMap`]): a lockstep walk pairs every statement-level AST node
//!   with its CST statement node, keyed by the analyzer's own node-path rendering
//!   (`body[3].then[1]`). [`RangeMap::resolve`] does longest-prefix lookup, so a diagnostic at a
//!   sub-expression path (`body[3].args[0]`) still lands on its statement's range.
//!
//! Acceptance agreement (legacy-accepted ⇒ ERROR-free CST) is enforced by the round-trip property
//! test and the `cst_agreement` corpus sweep, so the two front-ends cannot drift apart silently.
//! The follow-up that retires the line machinery entirely (tree-driven content lowering) is the
//! documented residual of L-59.

use std::collections::BTreeMap;

use rowan::TextRange;

use crate::ast::{DraftAst, Node};
use crate::error::Result;
use crate::parser::{parse_cst, Parse};
use crate::syntax::{SyntaxKind, SyntaxNode};

/// A lowering problem with a real source span (parser/lexer errors surfaced strictly).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowerError {
    pub message: String,
    pub range: Option<TextRange>,
}

/// Node-path → source-range side-map. Keys use the analyzer's path rendering: `body[0]`,
/// `body[2].then[1]`, `body[4].branches[0].body[0]`, … — statement-level granularity.
#[derive(Debug, Clone, Default)]
pub struct RangeMap {
    map: BTreeMap<String, TextRange>,
}

impl RangeMap {
    /// Exact lookup.
    pub fn get(&self, path: &str) -> Option<TextRange> {
        self.map.get(path).copied()
    }

    /// Longest-prefix lookup: `body[3].args[0].value` resolves to `body[3]`'s range when no finer
    /// entry exists. Prefixes are cut at `.` boundaries only, so `body[31]` never matches `body[3]`.
    pub fn resolve(&self, path: &str) -> Option<TextRange> {
        let mut p = path;
        loop {
            if let Some(r) = self.map.get(p) {
                return Some(*r);
            }
            p = &p[..p.rfind('.')?];
        }
    }

    /// Resolve the range for an analyzer diagnostic message, which renders its node path as a
    /// ``(at `body[3].then[1]`)`` suffix. Returns `None` for diagnostics without a path.
    pub fn resolve_diagnostic(&self, message: &str) -> Option<TextRange> {
        let start = message.rfind("(at `")? + "(at `".len();
        let end = message[start..].find('`')? + start;
        self.resolve(&message[start..end])
    }

    fn insert(&mut self, path: String, range: TextRange) {
        self.map.insert(path, range);
    }
}

/// A lowered flow: the semantic AST plus its statement-level range side-map.
#[derive(Debug, Clone)]
pub struct Lowered {
    pub ast: DraftAst,
    pub ranges: RangeMap,
}

/// Strict CST lowering: any lexer/parser error fails with spans; a clean tree lowers to the exact
/// legacy `DraftAst` (semantic authority: the shared line machinery) plus the range side-map.
pub fn cst_to_draft(parse: &Parse, src: &str) -> std::result::Result<Lowered, Vec<LowerError>> {
    if !parse.errors.is_empty() {
        return Err(parse
            .errors
            .iter()
            .map(|e| LowerError {
                message: e.message.clone(),
                range: Some(e.range),
            })
            .collect());
    }
    let ast = crate::parse::parse_flow_text(src).map_err(|e| {
        vec![LowerError {
            range: legacy_error_range(&e.to_string(), src),
            message: e.to_string(),
        }]
    })?;
    let mut ranges = RangeMap::default();
    map_flow(&parse.syntax(), &ast, &mut ranges);
    Ok(Lowered { ast, ranges })
}

/// The range-bearing flow front-end: legacy semantics/errors (pinned texts), CST-derived ranges.
/// Costs one extra (CST) parse over [`crate::parse::parse`], so it is for callers that consume
/// the ranges — the LSP. Acceptance drift between the two front-ends is enforced by the dedicated
/// test guards (`cst_agreement`, the round-trip property test), not asserted per parse: an
/// assertion here aborted debug builds on untrusted model-emitted text (review, 2026-07-09).
pub fn parse_with_ranges(src: &str) -> Result<Lowered> {
    let ast = crate::parse::parse_flow_text(src)?;
    let parse = parse_cst(src);
    let mut ranges = RangeMap::default();
    map_flow(&parse.syntax(), &ast, &mut ranges);
    Ok(Lowered { ast, ranges })
}

/// Best-effort range for a legacy `line N: …` error message: the whole 1-based line `N`.
fn legacy_error_range(message: &str, src: &str) -> Option<TextRange> {
    let rest = message.strip_prefix("line ")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let line: usize = digits.parse().ok()?;
    let mut start = 0usize;
    for (i, l) in src.split_inclusive('\n').enumerate() {
        if i + 1 == line {
            let end = start + l.trim_end_matches(['\r', '\n']).len();
            return Some(TextRange::new((start as u32).into(), (end as u32).into()));
        }
        start += l.len();
    }
    None
}

// ---------------------------------------------------------------------------
// The lockstep walk: DraftAst nodes ↔ CST statement nodes
// ---------------------------------------------------------------------------

/// Whether a CST node kind is a statement (a direct child of a BLOCK that corresponds to one
/// `DraftAst` body node). `EFFECT_ANNOT` is handled by merging; `UNTIL_CLAUSE`/`CTX_ENTRY` are
/// header/sub-line material owned by their parent statement.
fn is_stmt_kind(k: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        k,
        BIND_STMT
            | CALL_STMT
            | WHEN_STMT
            | UNLESS_STMT
            | EACH_STMT
            | REPEAT_STMT
            | MATCH_STMT
            | ROUTE_STMT
            | FALLBACK_STMT
            | PARALLEL_STMT
            | LOOP_STMT
            | TIMEOUT_STMT
            | BUDGET_STMT
            | WITH_TOOLS_STMT
            | RETRY_STMT
            | SEQ_STMT
            | CTX_STMT
            | CTX_APPEND_STMT
            | RETURN_STMT
            | ASSERT_STMT
            | JSON_ESCAPE
            | MEMO_STMT
            | ONCE_STMT
            | CHECKPOINT_STMT
            | AWAIT_STMT
            | CONFIRM_STMT
            | THROTTLE_STMT
            | DEBOUNCE_STMT
            | VERIFY_STMT
            | TRY_STMT
            | RACE_STMT
            | SCOPE_STMT
            | SAGA_STMT
            | PIPE_STMT
    )
}

/// The statement children of a BLOCK, with each `@effect` annotation line merged into the
/// statement it annotates (one `DraftAst` node) — range = annotation start → statement end.
fn stmt_children(block: &SyntaxNode) -> Vec<(SyntaxNode, TextRange)> {
    let mut out: Vec<(SyntaxNode, TextRange)> = Vec::new();
    let mut pending_annot: Option<TextRange> = None;
    for child in block.children() {
        let k = child.kind();
        if k == SyntaxKind::EFFECT_ANNOT {
            pending_annot = Some(child.text_range());
            continue;
        }
        if is_stmt_kind(k) {
            let range = match pending_annot.take() {
                Some(a) => TextRange::new(a.start(), child.text_range().end()),
                None => child.text_range(),
            };
            out.push((child, range));
        }
    }
    out
}

/// Map the (single) flow of this tree: pair `ast.body` with the first FLOW_DECL's BLOCK.
fn map_flow(root: &SyntaxNode, ast: &DraftAst, ranges: &mut RangeMap) {
    let Some(flow) = root.children().find(|c| c.kind() == SyntaxKind::FLOW_DECL) else {
        return;
    };
    let block = flow.children().find(|c| c.kind() == SyntaxKind::BLOCK);
    pair_block(block.as_ref(), &ast.body, "body", ranges);
}

/// Pair a `DraftAst` body slice with a BLOCK's statement children, positionally. On a count
/// mismatch the subtree is skipped — the side-map is best-effort by contract (the guards keep the
/// front-ends agreeing, so in practice this only skips exotic `@json`-embedded sub-bodies, which
/// have no distinct source lines of their own anyway).
fn pair_block(block: Option<&SyntaxNode>, nodes: &[Node], prefix: &str, ranges: &mut RangeMap) {
    let Some(block) = block else { return };
    let stmts = stmt_children(block);
    if stmts.len() != nodes.len() {
        return;
    }
    for (i, (node, (stmt, range))) in nodes.iter().zip(stmts.iter()).enumerate() {
        let path = format!("{prefix}[{i}]");
        ranges.insert(path.clone(), *range);
        recurse(node, stmt, &path, ranges);
    }
}

/// First direct BLOCK child of `n`.
fn block_of(n: &SyntaxNode) -> Option<SyntaxNode> {
    n.children().find(|c| c.kind() == SyntaxKind::BLOCK)
}

/// First direct child of kind `k`.
fn child_of(n: &SyntaxNode, k: SyntaxKind) -> Option<SyntaxNode> {
    n.children().find(|c| c.kind() == k)
}

/// Recurse into a statement's sub-blocks, mirroring the analyzer's path segments. `@json`-escaped
/// statements have no CST sub-blocks, so their inner bodies simply stay unmapped (they share the
/// escape line's range via prefix resolution).
fn recurse(node: &Node, stmt: &SyntaxNode, path: &str, ranges: &mut RangeMap) {
    match node {
        Node::When {
            then, otherwise, ..
        } => {
            pair_block(
                block_of(stmt).as_ref(),
                then,
                &format!("{path}.then"),
                ranges,
            );
            let else_block = child_of(stmt, SyntaxKind::ELSE_CLAUSE).and_then(|e| block_of(&e));
            pair_block(
                else_block.as_ref(),
                otherwise,
                &format!("{path}.otherwise"),
                ranges,
            );
        }
        Node::Unless { body, .. }
        | Node::Timeout { body, .. }
        | Node::Budget { body, .. }
        | Node::CapScope { body, .. }
        | Node::Retry { body, .. }
        | Node::Seq { body, .. }
        | Node::Once { body, .. }
        | Node::Confirm { body, .. }
        | Node::Throttle { body, .. }
        | Node::Debounce { body, .. }
        | Node::Each { body, .. }
        | Node::Repeat { body, .. }
        | Node::Loop { body, .. } => {
            pair_block(
                block_of(stmt).as_ref(),
                body,
                &format!("{path}.body"),
                ranges,
            );
        }
        Node::Try { body, handler, .. } => {
            pair_block(
                block_of(stmt).as_ref(),
                body,
                &format!("{path}.body"),
                ranges,
            );
            let catch_block = child_of(stmt, SyntaxKind::CATCH_CLAUSE).and_then(|c| block_of(&c));
            pair_block(
                catch_block.as_ref(),
                handler,
                &format!("{path}.handler"),
                ranges,
            );
        }
        Node::Scope { body, finally, .. } => {
            pair_block(
                block_of(stmt).as_ref(),
                body,
                &format!("{path}.body"),
                ranges,
            );
            let fin_block = child_of(stmt, SyntaxKind::FINALLY_CLAUSE).and_then(|f| block_of(&f));
            pair_block(
                fin_block.as_ref(),
                finally,
                &format!("{path}.finally"),
                ranges,
            );
        }
        Node::Parallel { branches } => {
            pair_branches(stmt, branches.iter().map(|b| &b.body), path, ranges)
        }
        Node::Race { branches, .. } => {
            pair_branches(stmt, branches.iter().map(|b| &b.body), path, ranges)
        }
        Node::Fallback { branches, .. } => {
            pair_branches(stmt, branches.iter().map(|b| &b.body), path, ranges)
        }
        Node::Match { cases, default, .. } => {
            pair_cases(stmt, cases.iter().map(|c| &c.body), default, path, ranges)
        }
        Node::Route { cases, default, .. } => {
            pair_cases(stmt, cases.iter().map(|c| &c.body), default, path, ranges)
        }
        Node::Saga { steps } => {
            let Some(block) = block_of(stmt) else { return };
            // STEP_ARM (+ optional trailing UNDO_CLAUSE) pairs, in order.
            let mut pairs: Vec<(SyntaxNode, Option<SyntaxNode>)> = Vec::new();
            for child in block.children() {
                match child.kind() {
                    SyntaxKind::STEP_ARM => pairs.push((child, None)),
                    SyntaxKind::UNDO_CLAUSE => {
                        if let Some(last) = pairs.last_mut() {
                            last.1 = Some(child);
                        }
                    }
                    _ => {}
                }
            }
            if pairs.len() != steps.len() {
                return;
            }
            for (j, (step, (arm, undo))) in steps.iter().zip(pairs.iter()).enumerate() {
                let spath = format!("{path}.steps[{j}]");
                let end = undo
                    .as_ref()
                    .map(|u| u.text_range().end())
                    .unwrap_or_else(|| arm.text_range().end());
                ranges.insert(spath.clone(), TextRange::new(arm.text_range().start(), end));
                pair_block(
                    block_of(arm).as_ref(),
                    &step.body,
                    &format!("{spath}.body"),
                    ranges,
                );
                let undo_block = undo.as_ref().and_then(block_of);
                pair_block(
                    undo_block.as_ref(),
                    &step.undo,
                    &format!("{spath}.undo"),
                    ranges,
                );
            }
        }
        Node::Pipe { steps, .. } => {
            pair_block(
                block_of(stmt).as_ref(),
                steps,
                &format!("{path}.steps"),
                ranges,
            );
        }
        // Header-only / expression statements: statement-level range is the finest source truth.
        _ => {}
    }
}

/// Pair CASE_ARM children with case bodies (+ the DEFAULT_ARM block): the shared lowering for
/// `match` and `route`, whose case structs differ in type but not in shape.
fn pair_cases<'a>(
    stmt: &SyntaxNode,
    bodies: impl Iterator<Item = &'a Vec<Node>>,
    default: &[Node],
    path: &str,
    ranges: &mut RangeMap,
) {
    let Some(block) = block_of(stmt) else { return };
    let arms: Vec<SyntaxNode> = block
        .children()
        .filter(|c| c.kind() == SyntaxKind::CASE_ARM)
        .collect();
    let bodies: Vec<&Vec<Node>> = bodies.collect();
    if arms.len() == bodies.len() {
        for (j, (body, arm)) in bodies.iter().zip(arms.iter()).enumerate() {
            let cpath = format!("{path}.cases[{j}]");
            ranges.insert(cpath.clone(), arm.text_range());
            pair_block(
                block_of(arm).as_ref(),
                body,
                &format!("{cpath}.body"),
                ranges,
            );
        }
    }
    let def_block = child_of(&block, SyntaxKind::DEFAULT_ARM).and_then(|d| block_of(&d));
    pair_block(
        def_block.as_ref(),
        default,
        &format!("{path}.default"),
        ranges,
    );
}

/// Pair BRANCH_ARM children with branch bodies: `{path}.branches[j]` + `.branches[j].body[i]`.
fn pair_branches<'a>(
    stmt: &SyntaxNode,
    bodies: impl Iterator<Item = &'a Vec<Node>>,
    path: &str,
    ranges: &mut RangeMap,
) {
    let Some(block) = block_of(stmt) else { return };
    let arms: Vec<SyntaxNode> = block
        .children()
        .filter(|c| c.kind() == SyntaxKind::BRANCH_ARM)
        .collect();
    let bodies: Vec<&Vec<Node>> = bodies.collect();
    if arms.len() != bodies.len() {
        return;
    }
    for (j, (body, arm)) in bodies.iter().zip(arms.iter()).enumerate() {
        let bpath = format!("{path}.branches[{j}]");
        ranges.insert(bpath.clone(), arm.text_range());
        pair_block(
            block_of(arm).as_ref(),
            body,
            &format!("{bpath}.body"),
            ranges,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RICH: &str = "flow f(count: Number) -> String\n  $status = git_status()\n  when $count > 3\n    $a = one()\n  else\n    $b = two()\n  try\n    risky()\n  catch $e\n    log($e)\n  parallel\n    branch $x\n      one()\n    branch $y\n      two()\n  saga\n    step\n      charge()\n    undo\n      refund()\n    step\n      ship()\n  match $status\n    case \"clean\"\n      done()\n    default\n      dirty()\n  return $status\n";

    #[test]
    fn lockstep_maps_statement_paths_to_their_lines() {
        let lowered = parse_with_ranges(RICH).expect("parses");
        let src = RICH;
        // body[0] = the bind on line 2.
        let r = lowered.ranges.get("body[0]").expect("body[0] mapped");
        assert!(
            src[r].contains("$status = git_status()"),
            "got {:?}",
            &src[r]
        );
        // when: then/otherwise blocks.
        let r = lowered.ranges.get("body[1].then[0]").expect("then[0]");
        assert!(src[r].contains("$a = one()"));
        let r = lowered
            .ranges
            .get("body[1].otherwise[0]")
            .expect("otherwise[0]");
        assert!(src[r].contains("$b = two()"));
        // try/catch.
        let r = lowered
            .ranges
            .get("body[2].handler[0]")
            .expect("handler[0]");
        assert!(src[r].contains("log($e)"));
        // parallel branches.
        let r = lowered
            .ranges
            .get("body[3].branches[1].body[0]")
            .expect("branch body");
        assert!(src[r].contains("two()"));
        // saga steps + undo.
        let r = lowered
            .ranges
            .get("body[4].steps[0].undo[0]")
            .expect("undo[0]");
        assert!(src[r].contains("refund()"));
        let r = lowered
            .ranges
            .get("body[4].steps[1].body[0]")
            .expect("step2 body");
        assert!(src[r].contains("ship()"));
        // match cases + default.
        let r = lowered
            .ranges
            .get("body[5].cases[0].body[0]")
            .expect("case body");
        assert!(src[r].contains("done()"));
        let r = lowered
            .ranges
            .get("body[5].default[0]")
            .expect("default[0]");
        assert!(src[r].contains("dirty()"));
    }

    #[test]
    fn resolve_falls_back_to_statement_prefix() {
        let lowered = parse_with_ranges(RICH).expect("parses");
        // A sub-expression path resolves to its statement's range.
        let stmt = lowered.ranges.get("body[0]").unwrap();
        assert_eq!(lowered.ranges.resolve("body[0].args[0]"), Some(stmt));
        assert_eq!(lowered.ranges.resolve("body[0].args[0].value"), Some(stmt));
        // …but an unrelated index never prefix-matches (`body[10]` ≠ `body[1]`).
        assert_eq!(lowered.ranges.resolve("nonexistent[0]"), None);
    }

    #[test]
    fn strict_cst_to_draft_errors_carry_ranges() {
        let src = "flow f\n  $x = (unclosed\n  return $x\n";
        let parse = parse_cst(src);
        let err = cst_to_draft(&parse, src).expect_err("strict on ERROR");
        assert!(!err.is_empty());
        assert!(
            err.iter().all(|e| e.range.is_some()),
            "every error has a span: {err:?}"
        );
    }

    #[test]
    fn effect_annotation_merges_into_the_bind_range() {
        let src = "flow f\n  @effect(read)\n  $x = read(\"a.txt\")\n  return $x\n";
        let lowered = parse_with_ranges(src).expect("parses");
        let r = lowered.ranges.get("body[0]").expect("bind mapped");
        let text = &src[r];
        assert!(
            text.contains("@effect(read)") && text.contains("$x = read"),
            "range spans annotation + bind: {text:?}"
        );
        // The return is body[1] — the annotation line did not shift pairing.
        assert!(src[lowered.ranges.get("body[1]").unwrap()].contains("return $x"));
    }
}
