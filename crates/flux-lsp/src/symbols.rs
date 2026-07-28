//! `symbols` — the document outline (L-68): each `flow`/`op` with its params and `$var` binds.

use flux_lang::syntax::SyntaxNode;
use tower_lsp::lsp_types::{DocumentSymbol, SymbolKind};

use crate::convert::{source_range, LineIndex};
use crate::scope::{collect_declarations, Def, DefRole};

pub fn document_symbols(root: &SyntaxNode, text: &str, index: &LineIndex) -> Vec<DocumentSymbol> {
    collect_declarations(root)
        .into_iter()
        .map(|(decl, members)| {
            let children: Vec<DocumentSymbol> = members
                .iter()
                .map(|m| member_symbol(m, text, index))
                .collect();
            #[allow(deprecated)]
            DocumentSymbol {
                name: decl.name,
                detail: None,
                kind: match decl.role {
                    DefRole::Op => SymbolKind::METHOD,
                    _ => SymbolKind::FUNCTION,
                },
                tags: None,
                deprecated: None,
                range: source_range(decl.full_range, text, index),
                selection_range: source_range(decl.name_range, text, index),
                children: (!children.is_empty()).then_some(children),
            }
        })
        .collect()
}

fn member_symbol(def: &Def, text: &str, index: &LineIndex) -> DocumentSymbol {
    let (name, kind, detail) = match def.role {
        DefRole::Param => (
            def.name.clone(),
            SymbolKind::VARIABLE,
            Some("parameter".into()),
        ),
        _ => (format!("${}", def.name), SymbolKind::VARIABLE, None),
    };
    #[allow(deprecated)]
    DocumentSymbol {
        name,
        detail,
        kind,
        tags: None,
        deprecated: None,
        range: source_range(def.full_range, text, index),
        selection_range: source_range(def.name_range, text, index),
        children: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp::lsp_types::Position;

    #[test]
    fn document_symbol_outlines_flow_params_and_binds() {
        let src = "flow greet(name: String)\n  $msg = fmt(\"hi\")\n  return $msg\n";
        let root = flux_lang::parser::parse_cst(src).syntax();
        let symbols = document_symbols(&root, src, &LineIndex::new(src));
        assert_eq!(symbols.len(), 1, "one top-level flow");
        let flow = &symbols[0];
        assert_eq!(flow.name, "greet");
        assert_eq!(flow.kind, SymbolKind::FUNCTION);
        assert_eq!(flow.selection_range.start, Position::new(0, 5));
        let children = flow.children.as_ref().expect("flow has member symbols");
        let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"name"), "param in outline: {names:?}");
        assert!(names.contains(&"$msg"), "bind in outline: {names:?}");
    }
}
