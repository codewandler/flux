//! `completion` — cursor-aware, scope-correct completion (L-85).
//!
//! Round 1 answered every request with the same list: every registered op, every node kind, every
//! prelude type, and every `$` byte-scanned out of the buffer — including variables bound in a
//! *different* flow, variables that only ever appear inside a string literal, and variables not yet
//! in scope. Go-to-definition, on the same buffer, was already scope-correct. Two answers to one
//! question, one of them wrong.
//!
//! Now the CST token at the cursor picks a [`Context`], and `$var` candidates come from the L-68
//! scope model — the same resolver go-to-definition uses. A completion list that omits an
//! out-of-scope `$var` is *correct* even though it is shorter.

use flux_lang::opspec::OpSignature;
use flux_lang::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, Documentation, InsertTextFormat, MarkupContent, MarkupKind,
};

use crate::scope::{all_var_defs, in_prose, resolve_var, Def, DefRole};

/// The annotations the grammar admits at statement level. Kept here as a small explicit table: the
/// set is part of the writable surface (`SyntaxKind::EFFECT_ANNOT` / `JSON_ESCAPE`), not something
/// the op registry knows about.
const ANNOTATIONS: &[(&str, &str)] = &[
    (
        "@effect",
        "Declare the effect tag of the statement below (`@effect(network)`).",
    ),
    (
        "@json",
        "Escape hatch: the next value is compact JSON in the wire format.",
    ),
];

/// What the cursor is asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    /// Inside a comment or a string literal — prose, so nothing is offered.
    Prose,
    /// Just after a `$` sigil: in-scope variables only.
    Var,
    /// Just after an `@` sigil: annotations only.
    Annotation,
    /// The first token of a line: the statement grammar (node kinds) plus ops.
    StatementHead,
    /// Inside a call's arguments or an object/list literal: ops, in-scope `$vars`, prelude types.
    Argument,
    /// Anywhere else in a statement: the full authoring surface.
    Expression,
}

/// The token to the *left* of the cursor — what the author has just typed.
fn left_token(root: &SyntaxNode, offset: usize) -> Option<SyntaxToken> {
    let ts = text_size::TextSize::from(offset as u32);
    root.token_at_offset(ts)
        .filter(|t| t.text_range().start() < ts)
        .last()
}

/// Classify the cursor from the CST.
pub fn context_at(root: &SyntaxNode, text: &str, offset: usize) -> Context {
    if in_prose(root, offset) {
        return Context::Prose;
    }
    let Some(tok) = left_token(root, offset) else {
        return Context::StatementHead;
    };
    // A bare `$`/`@` may not have lexed into a VAR/ANNOTATION yet, so read the byte too.
    match text[..offset].chars().next_back() {
        Some('$') => return Context::Var,
        Some('@') => return Context::Annotation,
        _ => {}
    }
    if tok.kind() == SyntaxKind::VAR {
        return Context::Var;
    }
    if tok.kind() == SyntaxKind::ANNOTATION {
        return Context::Annotation;
    }
    // A statement head: only whitespace between the start of the line and this token.
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let token_start = usize::from(tok.text_range().start());
    if token_start >= line_start && text[line_start..token_start].trim().is_empty() {
        return Context::StatementHead;
    }
    let in_args = tok.parent().into_iter().flat_map(|p| p.ancestors()).any(|n| {
        matches!(
            n.kind(),
            SyntaxKind::ARG_LIST
                | SyntaxKind::CALL_EXPR
                | SyntaxKind::OBJ_EXPR
                | SyntaxKind::LIST_EXPR
                | SyntaxKind::PARAM_LIST
        )
    });
    if in_args {
        Context::Argument
    } else {
        Context::Expression
    }
}

/// The in-scope `$var` bindings at `offset`, one entry per visible name with the innermost
/// (shadowing) binding winning — the same resolution go-to-definition performs.
pub fn visible_vars(root: &SyntaxNode, offset: usize) -> Vec<Def> {
    let defs = all_var_defs(root);
    let mut names: Vec<&str> = defs
        .iter()
        .filter(|d| {
            d.scope
                .contains_inclusive(text_size::TextSize::from(offset as u32))
        })
        .map(|d| d.name.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    names
        .into_iter()
        .filter_map(|name| resolve_var(&defs, name, offset).cloned())
        .collect()
}

/// The full markdown card for an op — shared with hover.
pub fn render_op(op: &OpSignature) -> String {
    let mut params = op.required_params.clone();
    let opt: Vec<String> = op.optional_params.iter().map(|p| format!("{p}?")).collect();
    params.extend(opt);
    format!(
        "**{}**({}) — {}\n\neffects: {:?} · risk: {:?} · idempotency: {:?}",
        op.name,
        params.join(", "),
        op.description,
        op.effects,
        op.risk,
        op.idempotency
    )
}

/// `read(${1:path})` — a snippet with one placeholder per required parameter, so accepting the
/// completion leaves the cursor in the first argument instead of after a bare `()`.
fn snippet(op: &OpSignature) -> String {
    if op.required_params.is_empty() {
        return format!("{}()", op.name);
    }
    let placeholders: Vec<String> = op
        .required_params
        .iter()
        .enumerate()
        .map(|(i, param)| format!("${{{}:{}}}", i + 1, param))
        .collect();
    format!("{}({})", op.name, placeholders.join(", "))
}

fn op_item(op: &OpSignature) -> CompletionItem {
    CompletionItem {
        label: op.name.clone(),
        kind: Some(CompletionItemKind::FUNCTION),
        detail: Some(op.description.clone()),
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: render_op(op),
        })),
        insert_text: Some(snippet(op)),
        insert_text_format: Some(InsertTextFormat::SNIPPET),
        ..Default::default()
    }
}

fn var_item(def: &Def) -> CompletionItem {
    CompletionItem {
        label: format!("${}", def.name),
        kind: Some(CompletionItemKind::VARIABLE),
        detail: Some(match def.role {
            DefRole::Param => "parameter".into(),
            _ => "bind".into(),
        }),
        ..Default::default()
    }
}

/// The completion list for `offset`.
pub fn completions(
    root: &SyntaxNode,
    text: &str,
    offset: usize,
    ops: &[OpSignature],
    node_kinds: &[(String, String)],
    prelude_types: &[(String, String)],
) -> Vec<CompletionItem> {
    let context = context_at(root, text, offset);
    let mut items = Vec::new();
    match context {
        Context::Prose => return items,
        Context::Var => {
            items.extend(visible_vars(root, offset).iter().map(var_item));
            return items;
        }
        Context::Annotation => {
            items.extend(ANNOTATIONS.iter().map(|(label, doc)| CompletionItem {
                label: (*label).into(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some((*doc).into()),
                ..Default::default()
            }));
            return items;
        }
        Context::StatementHead => {
            items.extend(node_kinds.iter().map(|(kind, doc)| CompletionItem {
                label: kind.clone(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some(doc.clone()),
                ..Default::default()
            }));
            items.extend(ops.iter().map(op_item));
        }
        Context::Argument => {
            items.extend(ops.iter().map(op_item));
            items.extend(visible_vars(root, offset).iter().map(var_item));
            items.extend(prelude_types.iter().map(|(ty, doc)| CompletionItem {
                label: ty.clone(),
                kind: Some(CompletionItemKind::STRUCT),
                detail: Some(doc.clone()),
                ..Default::default()
            }));
        }
        Context::Expression => {
            items.extend(ops.iter().map(op_item));
            items.extend(visible_vars(root, offset).iter().map(var_item));
            items.extend(node_kinds.iter().map(|(kind, doc)| CompletionItem {
                label: kind.clone(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some(doc.clone()),
                ..Default::default()
            }));
            items.extend(prelude_types.iter().map(|(ty, doc)| CompletionItem {
                label: ty.clone(),
                kind: Some(CompletionItemKind::STRUCT),
                detail: Some(doc.clone()),
                ..Default::default()
            }));
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::authoring_op_signatures;

    fn complete(src: &str, offset: usize) -> Vec<CompletionItem> {
        let root = flux_lang::parser::parse_cst(src).syntax();
        completions(
            &root,
            src,
            offset,
            &authoring_op_signatures(),
            &flux_lang::schema::node_kind_rows(),
            &flux_lang::prelude::prelude_type_rows(),
        )
    }

    fn labels(items: &[CompletionItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn a_cursor_in_one_flow_does_not_see_another_flows_binds() {
        let src = "flow a\n  $only_in_a = 1\n  return $only_in_a\n\nflow b\n  $here = 2\n  return $\n";
        let items = complete(src, src.rfind("return $").unwrap() + 8);
        let labels = labels(&items);
        assert!(labels.contains(&"$here"), "flow b's own bind: {labels:?}");
        assert!(
            !labels.contains(&"$only_in_a"),
            "flow a's bind is out of scope: {labels:?}"
        );
    }

    #[test]
    fn after_a_sigil_only_variables_are_offered() {
        let src = "flow f\n  $x = 1\n  return $\n";
        let items = complete(src, src.rfind('$').unwrap() + 1);
        assert_eq!(labels(&items), vec!["$x"], "only in-scope variables");
        assert!(items.iter().all(|i| i.kind == Some(CompletionItemKind::VARIABLE)));
    }

    #[test]
    fn a_name_that_only_appears_in_a_string_is_never_offered() {
        // The old byte scan collected `$ghost` out of the string literal.
        let src = "flow f\n  $x = fmt(\"spooky $ghost\")\n  return $\n";
        let items = complete(src, src.rfind('$').unwrap() + 1);
        let labels = labels(&items);
        assert!(labels.contains(&"$x"));
        assert!(!labels.contains(&"$ghost"), "string interior is not scope: {labels:?}");
    }

    #[test]
    fn a_cursor_inside_a_comment_or_string_offers_nothing() {
        let src = "flow f\n  # write something here\n  $x = fmt(\"read the manual\")\n  return $x\n";
        assert!(complete(src, src.find("something").unwrap() + 4).is_empty());
        assert!(complete(src, src.find("the manual").unwrap() + 4).is_empty());
    }

    #[test]
    fn a_statement_head_offers_keywords_and_ops_but_not_prelude_types() {
        let src = "flow f\n  $x = 1\n  re\n";
        let items = complete(src, src.rfind("re").unwrap() + 2);
        let labels = labels(&items);
        assert!(labels.contains(&"return"), "node kinds: {labels:?}");
        assert!(labels.contains(&"read"), "ops are callable at a head");
        let prelude = flux_lang::prelude::prelude_type_rows();
        if let Some((ty, _)) = prelude.first() {
            assert!(
                !labels.contains(&ty.as_str()),
                "a prelude type is not a statement: {labels:?}"
            );
        }
    }

    #[test]
    fn an_argument_position_offers_ops_and_in_scope_vars() {
        let src = "flow f\n  $path = \"a.txt\"\n  $body = read()\n  return $body\n";
        let items = complete(src, src.find("read(").unwrap() + 5);
        let labels = labels(&items);
        assert!(labels.contains(&"$path"), "in-scope var: {labels:?}");
        assert!(labels.contains(&"write"), "ops nest: {labels:?}");
    }

    #[test]
    fn op_items_carry_documentation_and_a_parameter_snippet() {
        let src = "flow f\n  re\n";
        let items = complete(src, src.rfind("re").unwrap() + 2);
        let read = items
            .iter()
            .find(|i| i.label == "read")
            .expect("read is offered");
        assert_eq!(read.insert_text_format, Some(InsertTextFormat::SNIPPET));
        assert!(
            read.insert_text.as_deref().is_some_and(|t| t.contains("${1:")),
            "snippet has a parameter placeholder: {:?}",
            read.insert_text
        );
        assert!(read.documentation.is_some(), "op card is attached");
    }

    #[test]
    fn an_inner_shadowing_bind_wins_over_the_outer_one() {
        let src = "flow f\n  $it = 0\n  each $it in $xs\n    do process $\n";
        let items = complete(src, src.rfind('$').unwrap() + 1);
        let its: Vec<&CompletionItem> = items.iter().filter(|i| i.label == "$it").collect();
        assert_eq!(its.len(), 1, "one entry per visible name: {:?}", labels(&items));
    }
}
