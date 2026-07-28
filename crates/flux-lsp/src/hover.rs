//! `hover` — CST-precise hover (L-86).
//!
//! Round 1 resolved the hovered word with a raw line scan, so `read` inside `# read the config` or
//! inside `"please read it"` rendered the `read` op card, and a `$var` never hovered at all — the
//! word set stopped at the `$`. Hover now resolves the **CST token** at the offset, refuses prose,
//! answers for `$vars` through the same scope model as go-to-definition, and returns the token's
//! `range` so clients can highlight what they answered about.

use flux_lang::opspec::OpSignature;
use flux_lang::syntax::{SyntaxKind, SyntaxNode};
use tower_lsp::lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Range};

use crate::completion::render_op;
use crate::convert::{source_range, LineIndex};
use crate::scope::{collect_declarations, symbol_at, DefRole, Symbol};

fn markdown(value: String, range: Range) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(range),
    }
}

/// The card for a `$var`: what kind of binding it is, which declaration owns it, and where it binds.
fn var_card(root: &SyntaxNode, symbol: &Symbol, text: &str, index: &LineIndex) -> String {
    let def = symbol.def();
    let owner = collect_declarations(root)
        .into_iter()
        .find(|(decl, _)| decl.full_range.contains_range(def.name_range))
        .map(|(decl, _)| match decl.role {
            DefRole::Op => format!("op `{}`", decl.name),
            _ => format!("flow `{}`", decl.name),
        })
        .unwrap_or_else(|| "this module".into());
    let role = match def.role {
        DefRole::Param => "parameter",
        _ => "bind",
    };
    let line = index
        .position(text, usize::from(def.name_range.start()))
        .line
        + 1;
    format!("**${}** ({role}) — {owner}\n\nbound at line {line}", def.name)
}

/// Hover for the token at `offset`, or `None` for prose, punctuation, and unknown identifiers.
pub fn hover_at(
    root: &SyntaxNode,
    text: &str,
    index: &LineIndex,
    offset: usize,
    ops: &[OpSignature],
    node_kinds: &[(String, String)],
    prelude_types: &[(String, String)],
) -> Option<Hover> {
    // `symbol_at` already refuses comments and string literals.
    if let Some(symbol) = symbol_at(root, offset) {
        let range = source_range(symbol.token_range(), text, index);
        return Some(match &symbol {
            Symbol::Var { .. } => markdown(var_card(root, &symbol, text, index), range),
            Symbol::Decl { def, .. } => {
                // A declaration in the buffer is also in the op catalog (composites) — prefer its
                // signature card, and fall back to naming the declaration.
                let card = ops
                    .iter()
                    .find(|op| op.name == def.name)
                    .map(render_op)
                    .unwrap_or_else(|| match def.role {
                        DefRole::Op => format!("**{}** (composite op)", def.name),
                        _ => format!("**{}** (flow)", def.name),
                    });
                markdown(card, range)
            }
        });
    }

    // Not a symbol the scope model knows: a host op, a grammar keyword, or a prelude type.
    let tok = crate::scope::token_at(root, offset)?;
    if crate::scope::in_prose(root, offset) || tok.kind() != SyntaxKind::IDENT {
        return None;
    }
    let (word, word_range) = match tok.parent() {
        Some(p) if p.kind() == SyntaxKind::NAME => (p.text().to_string(), p.text_range()),
        _ => (tok.text().to_string(), tok.text_range()),
    };
    let range = source_range(word_range, text, index);
    if let Some(op) = ops.iter().find(|o| o.name == word) {
        return Some(markdown(render_op(op), range));
    }
    if let Some((kind, doc)) = node_kinds.iter().find(|(k, _)| *k == word) {
        return Some(markdown(format!("**{kind}** (node kind)\n\n{doc}"), range));
    }
    if let Some((ty, doc)) = prelude_types.iter().find(|(t, _)| *t == word) {
        return Some(markdown(format!("**{ty}** (prelude type)\n\n{doc}"), range));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::authoring_op_signatures;

    fn hover(src: &str, offset: usize) -> Option<Hover> {
        let root = flux_lang::parser::parse_cst(src).syntax();
        let index = LineIndex::new(src);
        hover_at(
            &root,
            src,
            &index,
            offset,
            &authoring_op_signatures(),
            &flux_lang::schema::node_kind_rows(),
            &flux_lang::prelude::prelude_type_rows(),
        )
    }

    fn text_of(hover: &Hover) -> String {
        match &hover.contents {
            HoverContents::Markup(m) => m.value.clone(),
            other => panic!("expected markdown, got {other:?}"),
        }
    }

    #[test]
    fn prose_does_not_hover() {
        let src = "flow f\n  # read the config\n  $x = fmt(\"please read it\")\n  return $x\n";
        assert!(
            hover(src, src.find("# read").unwrap() + 3).is_none(),
            "a word inside a comment is not an op"
        );
        assert!(
            hover(src, src.find("please read it").unwrap() + 8).is_none(),
            "a word inside a string is not an op"
        );
    }

    #[test]
    fn a_var_use_hovers_its_binding() {
        let src = "flow f\n  $draft = 1\n  return $draft\n";
        let hover = hover(src, src.rfind("$draft").unwrap() + 2).expect("hovers");
        let card = text_of(&hover);
        assert!(card.contains("$draft"), "{card}");
        assert!(card.contains("bind"), "{card}");
        assert!(card.contains("flow `f`"), "names its declaration: {card}");
        assert!(card.contains("line 2"), "names its bind site: {card}");
        assert!(hover.range.is_some(), "hover carries the token range");
    }

    #[test]
    fn a_param_hovers_as_a_parameter() {
        let src = "flow greet(name: String)\n  return $name\n";
        let card = text_of(&hover(src, src.rfind("$name").unwrap() + 2).expect("hovers"));
        assert!(card.contains("parameter"), "{card}");
    }

    #[test]
    fn an_op_hover_still_renders_its_signature() {
        let src = "flow f\n  $x = read(\"a.txt\")\n  return $x\n";
        let card = text_of(&hover(src, src.find("read").unwrap() + 2).expect("hovers"));
        assert!(card.contains("**read**"), "{card}");
        assert!(card.contains("effects:") && card.contains("risk:"), "{card}");
    }

    #[test]
    fn a_node_kind_keyword_still_hovers() {
        let src = "flow f\n  return 1\n";
        let card = text_of(&hover(src, src.find("return").unwrap() + 2).expect("hovers"));
        assert!(card.contains("node kind"), "{card}");
    }

    #[test]
    fn the_hover_range_covers_the_token() {
        let src = "flow f\n  $x = read(\"a.txt\")\n  return $x\n";
        let hover = hover(src, src.find("read").unwrap() + 2).expect("hovers");
        let range = hover.range.expect("range");
        assert_eq!(range.start.line, 1);
        assert_eq!(range.end.character - range.start.character, 4);
    }
}
