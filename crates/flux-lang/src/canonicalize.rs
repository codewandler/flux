//! `canonicalize` — rewrite any accepted Flux-Lang dialect into the canonical one (L-103).
//!
//! [`crate::format_cst`] is a *layout* formatter: it recomputes indentation and interior spacing and
//! is otherwise faithful to the tokens the author wrote. That is the right contract for an editor's
//! "format document", and it is deliberately left alone here.
//!
//! This module is the other half — the **migration** pass. The parser accepts several spellings of
//! the same construct (`docs/designs/flux-syntax-simplification.md` tabulates them); only one of them
//! is canonical, and nothing pushed an author toward it because the canonical projection lived on
//! [`crate::format`], which formats a *semantic* [`crate::ast::DraftAst`] and so has already dropped
//! every comment. Reformatting a human's file through the AST is therefore not an option.
//!
//! So canonicalization is expressed as **byte-range splices over the lossless CST**. Comments,
//! blank-line structure and declaration order are never rebuilt — they are simply not edited — and
//! what lands in the buffer afterwards is [`crate::format_cst`]'s layout pass over the spliced text.
//!
//! # The rewrites
//! Each entry is a spelling the parser accepts today; the canonical column is what
//! [`crate::format`] emits for the same AST, which is what makes it canonical.
//!
//! | Legacy | Canonical |
//! | --- | --- |
//! | `$x = f($y)` | `x = f(y)` |
//! | `grep({ pattern: "x" })` | `grep(pattern: "x")` |
//! | `do poll` / `do file_bug $u` | `poll()` / `file_bug(u)` |
//! | `retry 3 backoff exponential delay 500` | `retry 3, backoff: exponential, delay: 500ms` |
//! | `loop for 5000 every 1000` | `loop for 5s, every: 1s` |
//! | `confirm "ok?" risk high` | `confirm "ok?", risk: high` |
//! | `throttle "f" 5 per 60000` | `throttle "f", max: 5, per: 1m` |
//! | `debounce "api" 500` | `debounce "api", wait: 500ms` |
//! | `timeout 30000` / `timeout 5000ms` / `timeout 60s` | `timeout 30s` / `timeout 5s` / `timeout 1m` |
//! | `race timeout: 5s` | `race 5s` |
//! | `with_tools [read_file]` | `with_tools ["read_file"]` |
//! | `repeat 10` + a body line `until $done` | `repeat 10, until: done` |
//! | `await $b = "src" when $c` | `await b = "src", when: c` |
//!
//! The table was built from the **decoder** (`cst_decode.rs`), not from the corpus. `throttle` and
//! `debounce` have legacy spellings that appear in no shipped file, so a corpus-driven rule set
//! misses them silently — and a formatter's false negative (reporting a legacy file as canonical)
//! is worse than a rejection, because the migration then skips it.
//!
//! # The guard
//! The rewrite is a *spelling* change and nothing else, so the output is held to the same
//! equivalence contract [`crate::format_cst::format_module`] carries: it must reparse without
//! errors, lower to the **same** [`crate::program::Module`], and carry the same comment multiset.
//! A rewrite that fails any of those is reported as [`Canonical::Rejected`] and the buffer is left
//! untouched — a formatter that silently drops a comment or moves a statement is worse than none.
//! Individual rules are additionally conservative where the CST cannot settle the question (a
//! `$`-sigil whose bare name is a statement keyword, an `until` line carrying a comment): they
//! decline rather than guess, because declining costs one un-migrated line and guessing costs the
//! whole file.

use crate::format::fmt_duration;
use crate::parser::Parse;
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// What canonicalizing a buffer produced.
///
/// The two failure arms are distinct on purpose: a caller reporting to a human needs to say
/// "this file does not parse" and "this file parses but the rewrite could not be proven equivalent"
/// differently — only the second is a defect in this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Canonical {
    /// The source is already canonical; no edit is needed.
    Unchanged,
    /// The canonical rewrite of the source.
    Rewritten(String),
    /// The source has parse errors; nothing was attempted.
    Unparsed,
    /// A rewrite was produced but failed the equivalence guard; the buffer must be left alone.
    Rejected,
}

impl Canonical {
    /// The canonical text, for a caller that treats "already canonical" and "rewritten" alike.
    /// `None` for the two failure arms.
    pub fn text(&self, original: &str) -> Option<String> {
        match self {
            Canonical::Unchanged => Some(original.to_string()),
            Canonical::Rewritten(text) => Some(text.clone()),
            Canonical::Unparsed | Canonical::Rejected => None,
        }
    }
}

/// Canonicalize `src` — rewrite every legacy spelling, then lay the result out with
/// [`crate::format_cst`].
pub fn canonicalize_source(src: &str) -> Canonical {
    canonicalize_module(&crate::parser::parse_cst(src), src)
}

/// Canonicalize an already-built CST. `text` must be the source `parsed` was built from.
pub fn canonicalize_module(parsed: &Parse, text: &str) -> Canonical {
    if !parsed.errors.is_empty() {
        return Canonical::Unparsed;
    }
    let root = parsed.syntax();
    let Ok(original) = crate::lower_cst::cst_to_module(parsed) else {
        return Canonical::Unparsed;
    };
    let original_comments = crate::format_cst::comment_multiset(&root);

    let spliced = apply(text, collect_edits(&root, text));

    // The layout pass owns indentation and interior spacing, so the splices above only ever have to
    // produce *correct* text, never pretty text.
    let relaid = crate::parser::parse_cst(&spliced);
    let out = crate::format_cst::format_module(&relaid).unwrap_or(spliced);

    if out == text {
        return Canonical::Unchanged;
    }

    // Equivalence guard: reparse clean, lower to the same module, keep every comment.
    let reparsed = crate::parser::parse_cst(&out);
    if !reparsed.errors.is_empty() {
        return Canonical::Rejected;
    }
    match crate::lower_cst::cst_to_module(&reparsed) {
        Ok(lowered) if lowered.module == original.module => {}
        _ => return Canonical::Rejected,
    }
    if crate::format_cst::comment_multiset(&reparsed.syntax()) != original_comments {
        return Canonical::Rejected;
    }
    Canonical::Rewritten(out)
}

/// One splice: replace `range` in the source with `text`.
type Edit = (std::ops::Range<usize>, String);

/// Apply `edits` to `src`. Edits are applied back-to-front so earlier ranges stay valid, and any
/// edit nested inside a wider one is dropped — the `until` hoist deletes a whole line that the sigil
/// rule may also have wanted to touch, and the hoist has already rewritten the text it carried up.
///
/// Ordering matters in two ways. A **zero-width insertion** sorts before a replacement starting at
/// the same offset, because the two do not conflict and both are wanted: `debounce "api" 500` needs
/// `, wait: ` inserted at the number *and* the number itself respelled as `500ms`. Among genuine
/// replacements the widest sorts first, so the containment test drops the nested one rather than the
/// container. Coverage is tracked as the furthest end reached, not merely the previous edit's, so a
/// zero-width insertion cannot mask the replacement in front of it.
fn apply(src: &str, mut edits: Vec<Edit>) -> String {
    edits.sort_by(|a, b| {
        a.0.start
            .cmp(&b.0.start)
            .then_with(|| a.0.is_empty().cmp(&b.0.is_empty()).reverse())
            .then_with(|| b.0.end.cmp(&a.0.end))
    });
    let mut kept: Vec<Edit> = Vec::with_capacity(edits.len());
    let mut covered = 0usize;
    for edit in edits {
        if edit.0.start < covered {
            continue;
        }
        covered = covered.max(edit.0.end);
        kept.push(edit);
    }
    let mut out = src.to_string();
    for (range, text) in kept.into_iter().rev() {
        out.replace_range(range, &text);
    }
    out
}

/// Walk the tree once and collect every rewrite.
fn collect_edits(root: &SyntaxNode, src: &str) -> Vec<Edit> {
    let mut edits = Vec::new();
    for node in root.descendants() {
        match node.kind() {
            SyntaxKind::ARG_LIST => unbrace_single_object_call(&node, &mut edits),
            SyntaxKind::CALL_STMT => desugar_do_call(&node, &mut edits),
            SyntaxKind::UNTIL_CLAUSE => hoist_until(&node, src, &mut edits),
            SyntaxKind::AWAIT_STMT => name_header_options(&node, &["when"], &mut edits),
            SyntaxKind::CONFIRM_STMT => name_header_options(&node, &["risk"], &mut edits),
            SyntaxKind::RETRY_STMT => name_header_options(&node, &["backoff", "delay"], &mut edits),
            SyntaxKind::LOOP_STMT => name_header_options(&node, &["every", "until"], &mut edits),
            SyntaxKind::RACE_STMT => positional_race_deadline(&node, &mut edits),
            // `throttle "f" 5 per 60000`: the max is bare, but the window always carries the `per`
            // keyword (`lower_throttle` requires it), so only the max needs a name.
            SyntaxKind::THROTTLE_STMT => {
                name_positional_operands(&node, &["max"], &mut edits);
                name_header_options(&node, &["per"], &mut edits);
            }
            SyntaxKind::DEBOUNCE_STMT => name_positional_operands(&node, &["wait"], &mut edits),
            SyntaxKind::WITH_TOOLS_STMT => quote_tool_names(&node, &mut edits),
            _ => {}
        }
        durations(&node, src, &mut edits);
    }
    for token in tokens(root) {
        if token.kind() == SyntaxKind::VAR {
            unsigil(&token, &mut edits);
        }
    }
    edits
}

/// Every token under `node`, in source order.
fn tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
}

/// The tokens `node` owns directly — the header run of a statement, excluding its body block.
fn child_tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|element| element.into_token())
}

/// A byte range from a rowan range.
fn span(token: &SyntaxToken) -> std::ops::Range<usize> {
    usize::from(token.text_range().start())..usize::from(token.text_range().end())
}

fn node_span(node: &SyntaxNode) -> std::ops::Range<usize> {
    usize::from(node.text_range().start())..usize::from(node.text_range().end())
}

// --- the rewrites -------------------------------------------------------------------------------

/// Is this `$sym` inside a **native formula**?
///
/// The sigil is not decoration there. `b = a * 2` is not merely unusual — the strict parser rejects
/// it with "expected a native expression", and [`crate::format`] emits `b = $a * 2` for that very
/// AST. So inside `BIN_EXPR`/`UNARY_EXPR` (and the parentheses wrapping one) the sigiled spelling
/// *is* the canonical one, and only outside them is it legacy. That line is the whole reason this
/// rule reads ancestors instead of rewriting every `VAR` token in the file.
fn in_native_formula(token: &SyntaxToken) -> bool {
    token
        .parent()
        .into_iter()
        .flat_map(|parent| parent.ancestors())
        .any(|node| {
            matches!(
                node.kind(),
                SyntaxKind::BIN_EXPR | SyntaxKind::UNARY_EXPR | SyntaxKind::PAREN_EXPR
            )
        })
}

/// Is this `$sym` one of **two or more** positional arguments of a call?
///
/// `f(a, b)` is not the positional call `f($a, $b)` — a run of bare identifiers in an argument list
/// is the *named-input pun* surface, so it lowers to named arguments `a: a, b: b` (and `f(a, a)` is
/// rejected outright as a duplicate named argument). `format` keeps the sigil in exactly this
/// position for the same reason — `format.rs`'s `fmt_call_args`: "Two or more bare variable
/// arguments are the named-input pun surface." A *single* argument is unambiguous and sheds it.
fn is_pun_ambiguous_argument(token: &SyntaxToken) -> bool {
    token
        .parent()
        .filter(|parent| parent.kind() == SyntaxKind::VAR_EXPR)
        .and_then(|var| var.parent())
        .is_some_and(|list| list.kind() == SyntaxKind::ARG_LIST && list.children().count() > 1)
}

/// Is this `$sym` an argument of a `do` call that keeps its `do` spelling?
///
/// `format` projects those through `fmt_legacy_call_args`, which sigils *every* symbol argument
/// regardless of how many there are — so `do fmt $x` is already canonical and stripping the `$`
/// would make this pass disagree with the formatter it defers to.
fn is_argument_of_a_retained_do_call(token: &SyntaxToken) -> bool {
    token
        .parent()
        .filter(|parent| parent.kind() == SyntaxKind::VAR_EXPR)
        .and_then(|var| var.parent())
        .filter(|list| list.kind() == SyntaxKind::ARG_LIST)
        .and_then(|list| list.parent())
        .is_some_and(|call| call.kind() == SyntaxKind::CALL_STMT && retains_do_spelling(&call))
}

/// `$x` → `x`. The sigil is legacy on an ordinary symbol reference (the canonical projection emits
/// bare identifiers); it stays wherever dropping it would be read as something else.
fn unsigil(token: &SyntaxToken, edits: &mut Vec<Edit>) {
    let bare = &token.text()[1..];
    if !crate::ast::is_bare_symbol_name(bare)
        || in_native_formula(token)
        || is_pun_ambiguous_argument(token)
        || is_argument_of_a_retained_do_call(token)
    {
        return;
    }
    edits.push((span(token), bare.to_string()));
}

/// `grep({ pattern: "x" })` → `grep(pattern: "x")`. The braces are dropped only for the exact shape
/// the legacy spelling produces: a lone object argument whose every field has an **identifier** key.
///
/// Two shapes that look similar are deliberately left alone, because for them the braces are not
/// notation but meaning:
/// - an *empty* object — `f({})` passes a value, `f()` passes nothing;
/// - **quoted** keys — `{ "channel": "cli" }` is valid JSON, so it lowers to a `lit` rather than to
///   an object template, and `f("channel": "cli")` is not a named argument list at all.
fn unbrace_single_object_call(arg_list: &SyntaxNode, edits: &mut Vec<Edit>) {
    let children: Vec<SyntaxNode> = arg_list.children().collect();
    let [obj] = children.as_slice() else { return };
    if obj.kind() != SyntaxKind::OBJ_EXPR {
        return;
    }
    let fields: Vec<SyntaxNode> = obj
        .children()
        .filter(|child| child.kind() == SyntaxKind::OBJ_FIELD)
        .collect();
    let identifier_keyed = |field: &SyntaxNode| {
        field
            .children()
            .find(|c| c.kind() == SyntaxKind::NAME)
            .is_some_and(|name| child_tokens(&name).any(|t| t.kind() == SyntaxKind::IDENT))
    };
    if fields.is_empty() || !fields.iter().all(identifier_keyed) {
        return;
    }
    // The call must be parenthesized already; a bare `do op { … }` argument list is not this shape.
    if !child_tokens(arg_list).any(|t| t.kind() == SyntaxKind::L_PAREN) {
        return;
    }
    for brace in
        child_tokens(obj).filter(|t| matches!(t.kind(), SyntaxKind::L_BRACE | SyntaxKind::R_BRACE))
    {
        edits.push((span(&brace), String::new()));
    }
}

/// The `do <name>` of a [`SyntaxKind::CALL_STMT`], if it has one.
fn do_keyword(call: &SyntaxNode) -> Option<(SyntaxToken, SyntaxNode)> {
    let kw = child_tokens(call).find(|t| !t.kind().is_trivia() && !t.kind().is_layout())?;
    if kw.kind() != SyntaxKind::IDENT || kw.text() != "do" {
        return None;
    }
    let name = call.children().find(|c| c.kind() == SyntaxKind::NAME)?;
    Some((kw, name))
}

/// Does this `do` call keep its `do` spelling?
///
/// `do fmt $x` / `do parse $x` / `do peek $x` are *canonical*, not legacy: those names are
/// special-cased pure spellings rather than op names, `format` emits the `do` form for them
/// unchanged, and `fmt($x)` is a different node. Rewriting one used to trip the equivalence guard,
/// which rejects per **file** — so a single such line made a whole file un-migratable, the exact
/// outcome this module's guard policy exists to avoid. `is_reserved_word` is the same
/// parser/formatter SSOT `unsigil` consults; declining on a genuinely op-named reserved word costs
/// one un-migrated line, which is the cheap side of that trade.
fn retains_do_spelling(call: &SyntaxNode) -> bool {
    do_keyword(call)
        .is_some_and(|(_, name)| crate::ast::is_reserved_word(name.text().to_string().trim()))
}

/// `do poll` → `poll()`, `do file_bug $u` → `file_bug($u)` (the sigil rule then takes the `$`).
fn desugar_do_call(call: &SyntaxNode, edits: &mut Vec<Edit>) {
    let Some((kw, name)) = do_keyword(call) else {
        return;
    };
    if retains_do_spelling(call) {
        return;
    }
    let name_end = node_span(&name).end;
    // Drop `do` and the whitespace behind it.
    edits.push((span(&kw).start..node_span(&name).start, String::new()));
    match call.children().find(|c| c.kind() == SyntaxKind::ARG_LIST) {
        Some(args) => {
            let args = node_span(&args);
            edits.push((name_end..args.start, "(".to_string()));
            edits.push((args.end..args.end, ")".to_string()));
        }
        None => edits.push((name_end..name_end, "()".to_string())),
    }
}

/// A body-line `until $done` under `repeat`/`loop` → a `, until: done` option on the header.
///
/// This is the only rule that *moves* a line, so it is the only one that can strand a comment, and
/// the equivalence guard cannot help: the comment multiset is order-insensitive, so a re-anchored
/// comment is invisible to it. Two shapes are therefore declined outright rather than guessed at —
/// a comment *on* the clause (there is no position for it on a header line) and a comment
/// *directly above* it (hoisting the clause out from under it silently re-aims the comment at the
/// next statement, turning a note about the loop guard into a note about whatever follows).
fn hoist_until(clause: &SyntaxNode, src: &str, edits: &mut Vec<Edit>) {
    if tokens(clause).any(|t| t.kind() == SyntaxKind::COMMENT) {
        return;
    }
    let clause_line = node_span(clause).start;
    let line_start = src[..clause_line].rfind('\n').map_or(0, |i| i + 1);
    if src[..line_start]
        .trim_end_matches('\n')
        .rsplit('\n')
        .next()
        .is_some_and(|above| above.trim_start().starts_with('#'))
    {
        return;
    }
    let Some(stmt) = clause.parent().and_then(|block| block.parent()) else {
        return;
    };
    if !matches!(stmt.kind(), SyntaxKind::REPEAT_STMT | SyntaxKind::LOOP_STMT) {
        return;
    }
    let Some(cond) = clause.children().next() else {
        return;
    };
    // The condition moves as text, so it is unsigiled here — the sigil rule's own edits fall inside
    // the deleted line and are dropped by `apply`.
    let condition: String = tokens(&cond)
        .map(|t| match t.kind() {
            SyntaxKind::VAR
                if crate::ast::is_bare_symbol_name(&t.text()[1..])
                    && !in_native_formula(&t)
                    && !is_pun_ambiguous_argument(&t) =>
            {
                t.text()[1..].to_string()
            }
            _ => t.text().to_string(),
        })
        .collect();
    if condition.trim().is_empty() {
        return;
    }

    // The header option goes before the `-> binding`, or at the end of the header line.
    let arrow = child_tokens(&stmt).find(|t| t.kind() == SyntaxKind::ARROW);
    let newline = child_tokens(&stmt).find(|t| t.kind() == SyntaxKind::NEWLINE);
    let Some(at) = arrow.or(newline).map(|t| span(&t).start) else {
        return;
    };
    edits.push((at..at, format!(", until: {}", condition.trim())));

    // Delete the clause's whole line, indentation and line break included.
    let clause = node_span(clause);
    let start = src[..clause.start].rfind('\n').map_or(0, |i| i + 1);
    edits.push((start..clause.end, String::new()));
}

/// The legacy space-keyword header: `retry 3 backoff exponential` → `retry 3, backoff: exponential`.
///
/// Only a *direct child* identifier counts — one already inside a [`SyntaxKind::HEADER_OPTION`] is
/// canonical and is not a child of the statement. `loop for 5s` keeps its bare `for`, which is why
/// the vocabulary is per-statement rather than a single shared set.
fn name_header_options(stmt: &SyntaxNode, options: &[&str], edits: &mut Vec<Edit>) {
    for token in child_tokens(stmt) {
        if token.kind() == SyntaxKind::IDENT && options.contains(&token.text()) {
            edits.push((span(&token), format!(", {}:", token.text())));
        }
    }
}

/// Give a statement's **positional** numeric operands their canonical option names, in order:
/// `throttle "f" 5 per 60000` → `throttle "f", max: 5, per: 60000`, `debounce "api" 500` →
/// `debounce "api", wait: 500`. (`durations` then spells the millisecond ones as literals.)
///
/// `lower_throttle`/`lower_debounce` (`cst_decode.rs`) read this operand run from reconstructed text
/// *and* accept the same fields as named options, which is what makes the positional form legacy.
/// Neither spelling occurs anywhere in the shipped corpus, so nothing but a rule written from the
/// decoder would have caught it.
///
/// Only direct-child numbers are positional — a number inside a [`SyntaxKind::HEADER_OPTION`] is
/// already named and is not a child of the statement.
fn name_positional_operands(stmt: &SyntaxNode, labels: &[&str], edits: &mut Vec<Edit>) {
    let numbers = child_tokens(stmt).filter(|t| t.kind() == SyntaxKind::NUMBER);
    for (number, label) in numbers.zip(labels) {
        let at = span(&number).start;
        edits.push((at..at, format!(", {label}: ")));
    }
}

/// `with_tools [read_file, write_file]` → `with_tools ["read_file", "write_file"]`.
///
/// `parse_setting_prefix` (`cst_decode.rs`) accepts a bare identifier wherever it wants a string, so
/// an unquoted tool name lowers identically to a quoted one — a doubled spelling with no marker in
/// the tree beyond the token kind. The elements are direct-child tokens of the statement (there is
/// no list node here), so the bracket run is tracked by hand.
///
/// `true`/`false`/`null` are *not* quoted: those lower to non-string values that the tool list then
/// rejects, and inventing a string for them would turn a loud error into a silent one.
fn quote_tool_names(stmt: &SyntaxNode, edits: &mut Vec<Edit>) {
    let mut in_list = false;
    for token in child_tokens(stmt) {
        match token.kind() {
            SyntaxKind::L_BRACK => in_list = true,
            SyntaxKind::R_BRACK => return,
            SyntaxKind::IDENT if in_list && !matches!(token.text(), "true" | "false" | "null") => {
                edits.push((span(&token), format!("\"{}\"", token.text())));
            }
            _ => {}
        }
    }
}

/// `race timeout: 5s` → `race 5s`. The deadline is positional in the canonical spelling; the named
/// alias is the only accepted form that spells the *same* operand two ways.
fn positional_race_deadline(stmt: &SyntaxNode, edits: &mut Vec<Edit>) {
    for option in stmt
        .children()
        .filter(|c| c.kind() == SyntaxKind::HEADER_OPTION)
    {
        let mut header = child_tokens(&option).filter(|t| !t.kind().is_trivia());
        let (Some(name), Some(colon)) = (header.next(), header.next()) else {
            continue;
        };
        if name.kind() == SyntaxKind::IDENT && name.text() == "timeout" {
            edits.push((span(&name).start..span(&colon).end, String::new()));
        }
    }
}

/// The option names whose value is a duration. A bare number in one of these positions means
/// milliseconds, and the canonical spelling is a duration literal (`500` → `500ms`, `30000` → `30s`).
const DURATION_OPTIONS: &[&str] = &["delay", "every", "for", "per", "timeout", "wait"];

/// `timeout 30000` → `timeout 30s`, `delay: 500` → `delay: 500ms`.
///
/// Only the positions the language actually reads as milliseconds are touched — `repeat 10`,
/// `budget 5`, `retry 3` and `max: 5` are counts, and giving one of those a unit would be a
/// different program.
fn durations(node: &SyntaxNode, src: &str, edits: &mut Vec<Edit>) {
    let numbers: Vec<SyntaxToken> = match node.kind() {
        // The deadline of these statements is their one positional operand.
        SyntaxKind::TIMEOUT_STMT | SyntaxKind::RACE_STMT => child_tokens(node)
            .find(|t| t.kind() == SyntaxKind::NUMBER)
            .into_iter()
            .collect(),
        // `loop for <ms> every <ms>` — both operands, in either the legacy or the canonical spelling.
        SyntaxKind::LOOP_STMT => child_tokens(node)
            .filter(|t| t.kind() == SyntaxKind::NUMBER)
            .collect(),
        // `retry 3 delay <ms>` — the count is the first number, the delay every number after it.
        // `throttle "f" 5 per <ms>` is the same shape: a leading count, then a window.
        SyntaxKind::RETRY_STMT | SyntaxKind::THROTTLE_STMT => child_tokens(node)
            .filter(|t| t.kind() == SyntaxKind::NUMBER)
            .skip(1)
            .collect(),
        // `debounce "api" <ms>` — its one positional operand is the wait.
        SyntaxKind::DEBOUNCE_STMT => child_tokens(node)
            .find(|t| t.kind() == SyntaxKind::NUMBER)
            .into_iter()
            .collect(),
        SyntaxKind::HEADER_OPTION => {
            let mut header = child_tokens(node).filter(|t| !t.kind().is_trivia());
            let name = header.find(|t| t.kind() == SyntaxKind::IDENT);
            match name {
                Some(name) if DURATION_OPTIONS.contains(&name.text()) => child_tokens(node)
                    .filter(|t| t.kind() == SyntaxKind::NUMBER)
                    .collect(),
                _ => return,
            }
        }
        _ => return,
    };
    for number in numbers {
        let Some((at, ms)) = duration_operand(&number) else {
            continue;
        };
        let canonical = fmt_duration(ms);
        if src.get(at.clone()) != Some(canonical.as_str()) {
            edits.push((at, canonical));
        }
    }
}

/// A duration operand's full byte span and its value in milliseconds.
///
/// A duration is a `NUMBER` plus an optional immediately-adjacent unit `IDENT` — `500ms` lexes as
/// two tokens the lexer never joins, while `500 ms` is two separate operands and not a duration at
/// all. **No suffix means milliseconds** (`take_duration`, `cst_decode.rs`), and `_` digit
/// separators are stripped before the value is read.
///
/// This normalizes the *unit* as well as supplying a missing one, because that is what canonical
/// means here: `fmt_duration` picks the largest exact unit, so `5000ms`, `5_000` and `5000` are all
/// spelled `5s`, and `60s` is spelled `1m`. Only supplying the missing suffix would leave
/// `timeout 5000ms` reported as already canonical when `format` disagrees.
fn duration_operand(number: &SyntaxToken) -> Option<(std::ops::Range<usize>, u64)> {
    let digits = number.text().replace('_', "");
    let value: u64 = digits.parse().ok()?;
    let unit = number.next_token().filter(|next| {
        next.kind() == SyntaxKind::IDENT && next.text_range().start() == number.text_range().end()
    });
    let (multiplier, end) = match unit.as_ref().map(|u| (u.text().to_string(), u)) {
        None => (1, span(number).end),
        Some((u, token)) => (
            match u.as_str() {
                "ms" => 1,
                "s" => 1_000,
                "m" => 60_000,
                // Not a duration unit — leave the operand alone rather than guess what it is.
                _ => return None,
            },
            span(token).end,
        ),
    };
    Some((span(number).start..end, value.checked_mul(multiplier)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canonicalize, asserting the rewrite was accepted.
    fn canon(src: &str) -> String {
        match canonicalize_source(src) {
            Canonical::Rewritten(out) => out,
            Canonical::Unchanged => src.to_string(),
            other => panic!("canonicalization failed: {other:?}\n{src}"),
        }
    }

    /// Wrap a body in a flow header.
    fn flow(body: &str) -> String {
        format!("flow f\n{body}")
    }

    #[test]
    fn strips_the_legacy_sigil_from_locals() {
        assert_eq!(
            canon(&flow("  $x = read(\"a.txt\")\n  return $x\n")),
            flow("  x = read(\"a.txt\")\n  return x\n")
        );
    }

    #[test]
    fn keeps_the_sigil_where_the_bare_name_is_a_keyword() {
        // `$until` is exactly what the sigil is for; stripping it would change how the line parses.
        let src = flow("  $until = 1\n  return $until\n");
        assert_eq!(canonicalize_source(&src), Canonical::Unchanged);
    }

    #[test]
    fn keeps_the_sigil_inside_a_native_formula() {
        // `b = a * 2` is not a tidier spelling of `b = $a * 2` — the strict parser rejects it with
        // "expected a native expression", and `format` emits the sigiled form for this very AST.
        // Inside a formula the sigil *is* canonical.
        let src = flow("  a = 1\n  b = $a * 2\n  c = !$b\n  return c\n");
        assert_eq!(canonicalize_source(&src), Canonical::Unchanged);
        assert_eq!(
            crate::format::format(&crate::parse::parse(&src).expect("the sigiled formula parses")),
            src,
            "the semantic formatter agrees this is already canonical"
        );
    }

    #[test]
    fn keeps_the_sigil_on_two_or_more_positional_arguments() {
        // A run of bare identifiers in an argument list is the *named-input pun* surface, so
        // `concat(a, b)` means `a: a, b: b` — and `concat(a, a)` is a duplicate-named-argument
        // error. One argument is unambiguous and sheds the sigil; two are not.
        let two = flow("  a = 1\n  c = concat($a, $a)\n  return c\n");
        assert_eq!(canonicalize_source(&two), Canonical::Unchanged);
        assert_eq!(
            canon(&flow("  a = 1\n  c = concat($a)\n  return c\n")),
            flow("  a = 1\n  c = concat(a)\n  return c\n")
        );
    }

    #[test]
    fn unwraps_a_braced_single_object_call_keeping_author_order() {
        // `format` sorts named inputs (they live in a BTreeMap); this pass rewrites *spelling* only,
        // so `pattern` stays in front of `glob` where the author put it.
        assert_eq!(
            canon(&flow(
                "  y = grep({ pattern: \"x\", glob: \"*.rs\" })\n  return y\n"
            )),
            flow("  y = grep(pattern: \"x\", glob: \"*.rs\")\n  return y\n")
        );
    }

    #[test]
    fn leaves_an_empty_object_argument_alone() {
        // `f({})` passes an empty object; `f()` passes nothing. Dropping the braces would change it.
        let src = flow("  y = build({})\n  return y\n");
        assert_eq!(canonicalize_source(&src), Canonical::Unchanged);
    }

    #[test]
    fn desugars_do_calls_with_and_without_arguments() {
        assert_eq!(
            canon(&flow("  do poll\n  do file_bug $u\n  return 1\n")),
            flow("  poll()\n  file_bug(u)\n  return 1\n")
        );
    }

    #[test]
    fn names_the_legacy_space_keyword_header_options() {
        assert_eq!(
            canon(&flow(
                "  retry 3 backoff exponential delay 500 -> out\n    flaky()\n  return out\n"
            )),
            flow(
                "  retry 3, backoff: exponential, delay: 500ms -> out\n    flaky()\n  return out\n"
            )
        );
        assert_eq!(
            canon(&flow(
                "  confirm \"ok?\" risk high\n    act()\n  return 1\n"
            )),
            flow("  confirm \"ok?\", risk: high\n    act()\n  return 1\n")
        );
        assert_eq!(
            canon(&flow(
                "  loop for 5000 every 1000 -> b\n    tick()\n  return b\n"
            )),
            flow("  loop for 5s, every: 1s -> b\n    tick()\n  return b\n")
        );
    }

    #[test]
    fn names_the_legacy_throttle_and_debounce_positional_operands() {
        // `lower_throttle`/`lower_debounce` (`cst_decode.rs`) accept a positional operand run
        // *alongside* the named options, so both statements have a legacy spelling — and neither
        // appears anywhere in the shipped corpus, which is exactly why the corpus test cannot see
        // this. A missed rule here is a silent false negative: `--check` would call the file clean.
        assert_eq!(
            canon(&flow(
                "  throttle \"fetches\" 5 per 60000\n    call()\n  return 1\n"
            )),
            flow("  throttle \"fetches\", max: 5, per: 1m\n    call()\n  return 1\n")
        );
        assert_eq!(
            canon(&flow("  debounce \"api\" 500\n    call()\n  return 1\n")),
            flow("  debounce \"api\", wait: 500ms\n    call()\n  return 1\n")
        );
    }

    #[test]
    fn leaves_the_pure_op_do_spellings_alone() {
        // `do fmt $x` is *canonical*, not legacy: `fmt`/`parse`/`peek`/`thing` are special-cased
        // pure spellings, `format` emits the `do` form for them unchanged, and rewriting one to
        // `fmt($x)` produces a different node. Before this rule the rewrite tripped the equivalence
        // guard, which rejects per **file** — so one such line made a whole file un-migratable.
        for src in [
            flow("  x = 1\n  do fmt $x\n  return 1\n"),
            flow("  x = \"{}\"\n  do parse $x\n  return 1\n"),
            flow("  x = 1\n  do peek $x\n  return 1\n"),
        ] {
            assert_eq!(
                canonicalize_source(&src),
                Canonical::Unchanged,
                "the pure-op `do` spelling is canonical: {src}"
            );
        }
        // A legacy `do` on an ordinary op, on the line right after, still canonicalizes.
        assert_eq!(
            canon(&flow("  x = 1\n  do fmt $x\n  do notify $x\n  return 1\n")),
            flow("  x = 1\n  do fmt $x\n  notify(x)\n  return 1\n")
        );
    }

    #[test]
    fn does_not_reanchor_a_comment_sitting_above_a_body_line_until() {
        // Hoisting the `until` out from under its own leading comment silently re-aims that comment
        // at the next statement. The comment *multiset* is order-insensitive, so only an assertion
        // on the text can see this — the guard never will.
        let src = flow(
            "  repeat 10 -> c\n    # why we stop early\n    until $done\n    step()\n  return c\n",
        );
        let out = canon(&src);
        assert!(
            out.contains("# why we stop early\n    until done"),
            "the comment keeps the statement it documents: {out}"
        );
        assert!(
            !out.contains("# why we stop early\n    step()"),
            "the comment must not be re-aimed at the next statement: {out}"
        );
    }

    #[test]
    fn spells_bare_millisecond_operands_as_durations() {
        assert_eq!(
            canon(&flow("  timeout 30000 -> o\n    slow()\n  return o\n")),
            flow("  timeout 30s -> o\n    slow()\n  return o\n")
        );
        assert_eq!(
            canon(&flow(
                "  debounce \"api\", wait: 500\n    call()\n  return 1\n"
            )),
            flow("  debounce \"api\", wait: 500ms\n    call()\n  return 1\n")
        );
    }

    #[test]
    fn normalizes_a_duration_unit_not_merely_a_missing_one() {
        // `fmt_duration` picks the largest exact unit, so a *present* unit can still be
        // non-canonical. Supplying only the missing suffix would report `timeout 5000ms` as already
        // canonical while `format` spells it `5s` — a false negative, the shape that matters most
        // for `--check`. `_` digit separators are stripped the way `take_duration` strips them.
        for (legacy, canonical) in [
            ("timeout 5000ms -> o", "timeout 5s -> o"),
            ("timeout 60s -> o", "timeout 1m -> o"),
            ("timeout 5_000 -> o", "timeout 5s -> o"),
            ("timeout 250 -> o", "timeout 250ms -> o"),
        ] {
            assert_eq!(
                canon(&flow(&format!("  {legacy}\n    slow()\n  return o\n"))),
                flow(&format!("  {canonical}\n    slow()\n  return o\n"))
            );
        }
        // 90000ms is 90s, not 1.5m — an inexact division keeps the smaller unit.
        assert_eq!(
            canon(&flow("  timeout 90000ms -> o\n    slow()\n  return o\n")),
            flow("  timeout 90s -> o\n    slow()\n  return o\n")
        );
    }

    #[test]
    fn quotes_bare_tool_names() {
        // `parse_setting_prefix` takes a bare identifier as a string, so the unquoted list is a
        // legacy spelling rather than a different program.
        assert_eq!(
            canon(&flow(
                "  with_tools [read_file, write_file]\n    act()\n  return 1\n"
            )),
            flow("  with_tools [\"read_file\", \"write_file\"]\n    act()\n  return 1\n")
        );
    }

    #[test]
    fn leaves_count_operands_alone() {
        // `repeat 10`, `budget 5`, `retry 3` and `max:` are counts, not milliseconds.
        let src = flow("  repeat 10 -> c\n    step()\n  return c\n");
        assert_eq!(canonicalize_source(&src), Canonical::Unchanged);
        let src = flow("  budget 5 -> b\n    act()\n  return b\n");
        assert_eq!(canonicalize_source(&src), Canonical::Unchanged);
    }

    #[test]
    fn hoists_a_body_line_until_into_the_header() {
        assert_eq!(
            canon(&flow(
                "  repeat 10 -> c\n    until $done\n    step()\n  return c\n"
            )),
            flow("  repeat 10, until: done -> c\n    step()\n  return c\n")
        );
        assert_eq!(
            canon(&flow(
                "  loop for 5000, every: 1000\n    until $done\n    step()\n  return 1\n"
            )),
            flow("  loop for 5s, every: 1s, until: done\n    step()\n  return 1\n")
        );
    }

    #[test]
    fn leaves_a_commented_until_line_where_the_author_put_it() {
        // Hoisting the clause would have to invent a position for the comment; declining costs one
        // un-migrated line, guessing costs the comment.
        let src =
            flow("  repeat 10 -> c\n    until $done  # give up early\n    step()\n  return c\n");
        let out = canon(&src);
        assert!(
            out.contains("until done  # give up early"),
            "the commented clause stays a body line: {out}"
        );
        assert!(!out.contains(", until:"), "and is not hoisted: {out}");
    }

    #[test]
    fn names_the_legacy_await_guard() {
        assert_eq!(
            canon(&flow("  await $b = \"src\" when $cond\n  return $b\n")),
            flow("  await b = \"src\", when: cond\n  return b\n")
        );
    }

    #[test]
    fn makes_the_race_deadline_positional() {
        assert_eq!(
            canon(&flow(
                "  race timeout: 5s -> b\n    branch one\n      a()\n    branch two\n      b()\n  return b\n"
            )),
            flow("  race 5s -> b\n    branch one\n      a()\n    branch two\n      b()\n  return b\n")
        );
    }

    #[test]
    fn is_idempotent() {
        let src = flow(
            "  $hits = grep({ pattern: \"x\" })\n  do notify $hits\n  timeout 30000 -> $o\n    slow()\n  return $o\n",
        );
        let once = canon(&src);
        assert_eq!(
            canonicalize_source(&once),
            Canonical::Unchanged,
            "a canonicalized buffer is a fixed point"
        );
    }

    #[test]
    fn refuses_to_touch_a_buffer_that_does_not_parse() {
        assert_eq!(
            canonicalize_source("flow x\n  confirm \"y\n"),
            Canonical::Unparsed
        );
    }

    #[test]
    fn keeps_every_comment_at_every_block_level() {
        let src = "\
# module
flow f
  # leading
  $x = 1  # trailing
  when $x
    # nested
    do act
  # dangling
  return $x
";
        let out = canon(src);
        let before = crate::format_cst::comment_multiset(&crate::parser::parse_cst(src).syntax());
        let after = crate::format_cst::comment_multiset(&crate::parser::parse_cst(&out).syntax());
        assert_eq!(before, after, "comment multiset changed:\n{out}");
        assert!(out.contains("x = 1  # trailing"), "{out}");
        assert!(out.contains("    # nested\n    act()"), "{out}");
    }
}
