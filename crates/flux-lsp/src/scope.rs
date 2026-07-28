//! `scope` — the CST scope model (L-68) and everything that resolves a cursor through it:
//! go-to-definition, find-references, and rename (L-87).
//!
//! One relation underpins all three. `collect_declarations` walks the top-level `flow`/`op` nodes
//! and their parameter/bind definitions; `resolve_var` maps a `$var` *use* to the innermost
//! same-named binding in scope at that offset. Go-to-definition is that relation; find-references is
//! it inverted (every use whose resolution is *this* binding); rename is find-references plus an
//! edit. Keeping them on one resolver is what stops the editor from giving two answers to the same
//! question — the bug L-85 fixed on the completion side.

use flux_lang::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use text_size::{TextRange, TextSize};

/// The role a definition plays — drives its LSP `SymbolKind` and how a use resolves to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefRole {
    Flow,
    Op,
    Param,
    Bind,
}

/// One definition site in the CST: a top-level `flow`/`op` declaration, a flow/op parameter, or a
/// `$var` bind (`bind`/`memo`/`each`/arrow-collect/`parallel`-branch/`catch`/`scope`). `scope` is the
/// source region in which the binding is visible, so a use resolves to the *innermost* same-named
/// binding that contains it.
#[derive(Debug, Clone)]
pub struct Def {
    pub name: String,
    pub role: DefRole,
    /// Range of the defining token (the name / `$var`) — the go-to-definition target.
    pub name_range: TextRange,
    /// The full declaration/statement range (a symbol's enclosing range).
    pub full_range: TextRange,
    /// The region in which this binding is in scope (for use → def resolution).
    pub scope: TextRange,
}

fn range_len(range: TextRange) -> u32 {
    u32::from(range.len())
}

/// First direct `$var` token child of `node` (bind targets, branch/catch/scope binders).
fn first_var_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .find(|t| t.kind() == SyntaxKind::VAR)
}

/// Every direct `$var` token child of `node` (the `each` loop + collect binders).
fn direct_var_tokens(node: &SyntaxNode) -> Vec<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::VAR)
        .collect()
}

/// The name + range of a declaration from its header, joining kebab-case segments (`god-review`).
/// `None` for an anonymous `flow`/`op` (no name token before `(` / `->` / newline).
fn decl_name(header: &SyntaxNode) -> Option<(String, TextRange)> {
    let toks: Vec<SyntaxToken> = header
        .children_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| !t.kind().is_trivia())
        .collect();
    // toks[0] is the `flow`/`op` keyword; the name (if any) is the next IDENT.
    let first = toks.get(1)?;
    if first.kind() != SyntaxKind::IDENT {
        return None;
    }
    let start = first.text_range().start();
    let mut end = first.text_range().end();
    let mut name = first.text().to_string();
    let mut i = 2;
    while i + 1 < toks.len()
        && toks[i].kind() == SyntaxKind::MINUS
        && matches!(toks[i + 1].kind(), SyntaxKind::IDENT | SyntaxKind::NUMBER)
        && toks[i].text_range().start() == end
    {
        name.push('-');
        name.push_str(toks[i + 1].text());
        end = toks[i + 1].text_range().end();
        i += 2;
    }
    Some((name, TextRange::new(start, end)))
}

/// Collect the flow/op parameter definitions from a header into `out` (visible across the decl).
fn collect_params(header: &SyntaxNode, decl_range: TextRange, out: &mut Vec<Def>) {
    let Some(list) = header
        .children()
        .find(|c| c.kind() == SyntaxKind::PARAM_LIST)
    else {
        return;
    };
    for param in list.children().filter(|c| c.kind() == SyntaxKind::PARAM) {
        // The param name is the first direct IDENT token (its type lives in a child NAME node).
        if let Some(name_tok) = param
            .children_with_tokens()
            .filter_map(|el| el.into_token())
            .find(|t| t.kind() == SyntaxKind::IDENT)
        {
            out.push(Def {
                name: name_tok.text().to_string(),
                role: DefRole::Param,
                name_range: name_tok.text_range(),
                full_range: param.text_range(),
                scope: decl_range,
            });
        }
    }
}

fn push_var_def(out: &mut Vec<Def>, tok: &SyntaxToken, full: TextRange, scope: TextRange) {
    let name = tok.text().trim_start_matches('$');
    if name.is_empty() {
        return;
    }
    out.push(Def {
        name: name.to_string(),
        role: DefRole::Bind,
        name_range: tok.text_range(),
        full_range: full,
        scope,
    });
}

/// Collect every `$var` binding inside a declaration's body. Ordinary `bind`/`memo` binds are
/// visible across the whole declaration; the narrower binders (`each` loop/collect vars,
/// `parallel`/`race`/`fallback` branch vars, `catch`, `scope`) scope to their own statement so a
/// shadowing use resolves to the inner binding.
fn collect_binds(decl: &SyntaxNode, decl_range: TextRange, out: &mut Vec<Def>) {
    for node in decl.descendants() {
        match node.kind() {
            SyntaxKind::BIND_STMT | SyntaxKind::MEMO_STMT => {
                if let Some(v) = first_var_token(&node) {
                    push_var_def(out, &v, node.text_range(), decl_range);
                }
            }
            SyntaxKind::EACH_STMT => {
                let scope = node.text_range();
                for v in direct_var_tokens(&node) {
                    push_var_def(out, &v, scope, scope);
                }
            }
            SyntaxKind::BRANCH_ARM | SyntaxKind::CATCH_CLAUSE | SyntaxKind::SCOPE_STMT => {
                let scope = node.text_range();
                if let Some(v) = first_var_token(&node) {
                    push_var_def(out, &v, scope, scope);
                }
            }
            _ => {}
        }
    }
}

/// Every top-level executable declaration with its member definitions (params + binds).
pub fn collect_declarations(root: &SyntaxNode) -> Vec<(Def, Vec<Def>)> {
    let mut out = Vec::new();
    for decl in root.children() {
        let (role, header_kind, default_name) = match decl.kind() {
            SyntaxKind::FLOW_DECL => (DefRole::Flow, SyntaxKind::FLOW_HEADER, "flow"),
            SyntaxKind::OP_DECL => (DefRole::Op, SyntaxKind::OP_HEADER, "op"),
            _ => continue,
        };
        let full_range = decl.text_range();
        let header = decl.children().find(|c| c.kind() == header_kind);
        let (name, name_range) = header
            .as_ref()
            .and_then(decl_name)
            .unwrap_or_else(|| (default_name.to_string(), full_range));
        let mut members = Vec::new();
        if let Some(header) = &header {
            collect_params(header, full_range, &mut members);
        }
        collect_binds(&decl, full_range, &mut members);
        out.push((
            Def {
                name,
                role,
                name_range,
                full_range,
                scope: full_range,
            },
            members,
        ));
    }
    out
}

/// Flat list of every `$var`/param definition across all declarations (for use → def resolution).
pub fn all_var_defs(root: &SyntaxNode) -> Vec<Def> {
    collect_declarations(root)
        .into_iter()
        .flat_map(|(_, members)| members)
        .collect()
}

/// The token covering (or adjacent to) `offset`, preferring a `$var`/identifier over trivia.
pub fn token_at(root: &SyntaxNode, offset: usize) -> Option<SyntaxToken> {
    let ts = TextSize::from(offset as u32);
    let candidates: Vec<SyntaxToken> = root.token_at_offset(ts).collect();
    candidates
        .iter()
        .find(|t| matches!(t.kind(), SyntaxKind::VAR | SyntaxKind::IDENT))
        .or_else(|| candidates.iter().find(|t| !t.kind().is_trivia()))
        .or_else(|| candidates.first())
        .cloned()
}

/// Is the offset inside a comment or a string literal — i.e. inside prose, not code? Completion and
/// hover must both say "nothing here" (L-85 / L-86); a raw word scan could not tell.
pub fn in_prose(root: &SyntaxNode, offset: usize) -> bool {
    let ts = TextSize::from(offset as u32);
    root.token_at_offset(ts).any(|tok| match tok.kind() {
        // A cursor *at* the `#` still sits in code position; past it, the rest of the line is prose.
        SyntaxKind::COMMENT => tok.text_range().start() < ts,
        // Strictly inside the quotes: the offsets on either delimiter are code position, so
        // completion still fires for the argument that follows a string.
        SyntaxKind::STRING => tok.text_range().start() < ts && ts < tok.text_range().end(),
        _ => false,
    })
}

/// Resolve a `$var` token to the innermost same-named binding in scope at `offset`. A token *at* a
/// bind site resolves to that bind, so uses and definitions share one identity.
pub fn resolve_var<'a>(defs: &'a [Def], name: &str, offset: usize) -> Option<&'a Def> {
    let use_off = TextSize::from(offset as u32);
    let mut best: Option<&Def> = None;
    for cand in defs {
        if cand.name != name || !cand.scope.contains_inclusive(use_off) {
            continue;
        }
        best = Some(match best {
            None => cand,
            Some(cur) if better_binding(cand, cur, use_off) => cand,
            Some(cur) => cur,
        });
    }
    best
}

/// Is binding `a` a better resolution than `b` for a use at `off`? Prefer the smaller (inner)
/// scope; within an equal scope prefer a binding defined at/before the use, latest first.
fn better_binding(a: &Def, b: &Def, off: TextSize) -> bool {
    let (la, lb) = (range_len(a.scope), range_len(b.scope));
    if la != lb {
        return la < lb;
    }
    let (a_before, b_before) = (a.name_range.start() <= off, b.name_range.start() <= off);
    if a_before != b_before {
        return a_before;
    }
    if a_before {
        a.name_range.start() > b.name_range.start()
    } else {
        a.name_range.start() < b.name_range.start()
    }
}

/// The dotted/kebab name an identifier token spells in reference position (`ai.extract`, `greet`).
fn ident_name(tok: &SyntaxToken) -> String {
    match tok.parent() {
        Some(p) if p.kind() == SyntaxKind::NAME => p.text().to_string(),
        _ => tok.text().to_string(),
    }
}

/// What the cursor is on, resolved through the scope model — the shared front half of
/// go-to-definition, find-references, and rename.
#[derive(Debug, Clone)]
pub enum Symbol {
    /// A `$var` use or bind, with the binding it belongs to.
    Var { def: Def, token: TextRange },
    /// A `flow`/`op` name, at its declaration or at a call site.
    Decl { def: Def, token: TextRange },
}

impl Symbol {
    /// The range of the token under the cursor (what `prepareRename` highlights).
    pub fn token_range(&self) -> TextRange {
        match self {
            Symbol::Var { token, .. } | Symbol::Decl { token, .. } => *token,
        }
    }

    /// The definition this symbol names.
    pub fn def(&self) -> &Def {
        match self {
            Symbol::Var { def, .. } | Symbol::Decl { def, .. } => def,
        }
    }
}

/// Resolve the cursor to a renameable symbol, or `None` for punctuation, a keyword, a literal, or a
/// position inside prose.
pub fn symbol_at(root: &SyntaxNode, offset: usize) -> Option<Symbol> {
    if in_prose(root, offset) {
        return None;
    }
    let tok = token_at(root, offset)?;
    match tok.kind() {
        SyntaxKind::VAR => {
            let name = tok.text().trim_start_matches('$');
            let defs = all_var_defs(root);
            let def = resolve_var(&defs, name, offset)?.clone();
            Some(Symbol::Var {
                def,
                token: tok.text_range(),
            })
        }
        SyntaxKind::IDENT => {
            let name = ident_name(&tok);
            let (def, _) = collect_declarations(root)
                .into_iter()
                .find(|(d, _)| d.name == name && matches!(d.role, DefRole::Op | DefRole::Flow))?;
            let token = match tok.parent() {
                Some(p) if p.kind() == SyntaxKind::NAME => p.text_range(),
                _ => tok.text_range(),
            };
            Some(Symbol::Decl { def, token })
        }
        _ => None,
    }
}

/// Every occurrence of `symbol` in this document: its definition plus every reference that resolves
/// to *that* definition — never a same-named binding in another scope or another declaration.
pub fn references(root: &SyntaxNode, symbol: &Symbol) -> Vec<TextRange> {
    let mut out = Vec::new();
    match symbol {
        Symbol::Var { def, .. } => {
            let defs = all_var_defs(root);
            for tok in root
                .descendants_with_tokens()
                .filter_map(|el| el.into_token())
                .filter(|t| t.kind() == SyntaxKind::VAR)
            {
                if tok.text().trim_start_matches('$') != def.name {
                    continue;
                }
                let at = usize::from(tok.text_range().start());
                if resolve_var(&defs, &def.name, at).map(|d| d.name_range) == Some(def.name_range) {
                    out.push(tok.text_range());
                }
            }
        }
        Symbol::Decl { def, .. } => {
            out.push(def.name_range);
            for name in root
                .descendants()
                .filter(|n| n.kind() == SyntaxKind::NAME)
                .filter(|n| n.text() == def.name.as_str())
            {
                out.push(name.text_range());
            }
        }
    }
    out.sort_by_key(|r| r.start());
    out.dedup();
    out
}

/// Go-to-definition: a `$var` use jumps to its binding; an op/flow reference to its declaration.
pub fn definition_at(root: &SyntaxNode, offset: usize) -> Option<TextRange> {
    symbol_at(root, offset).map(|symbol| symbol.def().name_range)
}

/// Is `name` a legal target for a rename of `symbol`? `$vars` and declaration names have different
/// spelling rules, and neither admits the `$` sigil in the typed text.
pub fn valid_new_name(symbol: &Symbol, name: &str) -> bool {
    let name = name.trim_start_matches('$');
    match symbol {
        Symbol::Var { .. } => flux_lang::ast::SymbolName(name.to_string()).is_identifier(),
        Symbol::Decl { .. } => flux_lang::ast::is_valid_decl_name(name),
    }
}

/// The replacement text for one occurrence — a `$var` keeps its sigil.
pub fn replacement_for(symbol: &Symbol, new_name: &str) -> String {
    let bare = new_name.trim_start_matches('$');
    match symbol {
        Symbol::Var { .. } => format!("${bare}"),
        Symbol::Decl { .. } => bare.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_of(src: &str) -> SyntaxNode {
        flux_lang::parser::parse_cst(src).syntax()
    }

    fn at(src: &str, needle: &str) -> usize {
        src.find(needle).unwrap_or_else(|| panic!("no {needle:?}")) + 1
    }

    #[test]
    fn references_stay_inside_the_binding_that_owns_them() {
        // Two flows each bind `$x`; the second flow's uses must not join the first's reference set.
        let src = "flow a\n  $x = 1\n  return $x\n\nflow b\n  $x = 2\n  return $x\n";
        let root = root_of(src);
        let symbol = symbol_at(&root, at(src, "$x = 1")).expect("resolves");
        let refs = references(&root, &symbol);
        assert_eq!(refs.len(), 2, "the bind and its one use: {refs:?}");
        let boundary = src.find("flow b").unwrap() as u32;
        assert!(
            refs.iter().all(|r| u32::from(r.start()) < boundary),
            "flow b's `$x` is a different binding: {refs:?}"
        );
    }

    #[test]
    fn references_respect_inner_shadowing() {
        let src = "flow f\n  $it = 0\n  each $it in $xs\n    do process $it\n  return $it\n";
        let root = root_of(src);
        let inner = symbol_at(&root, at(src, "each $it") + 5).expect("resolves the each binder");
        let refs = references(&root, &inner);
        assert_eq!(refs.len(), 2, "the each binder and the use inside it: {refs:?}");
        let outer = symbol_at(&root, at(src, "$it = 0")).expect("resolves the flow bind");
        let outer_refs = references(&root, &outer);
        assert_eq!(
            outer_refs.len(),
            2,
            "the flow bind and the trailing use: {outer_refs:?}"
        );
    }

    #[test]
    fn a_composite_name_references_its_declaration_and_every_call_site() {
        let src = "op greet(name: String) -> String\n  return $name\n\nflow one\n  return greet(\"a\")\n\nflow two\n  return greet(\"b\")\n";
        let root = root_of(src);
        let symbol = symbol_at(&root, at(src, "greet(\"a\")")).expect("resolves");
        let refs = references(&root, &symbol);
        assert_eq!(refs.len(), 3, "declaration + two call sites: {refs:?}");
    }

    #[test]
    fn punctuation_and_prose_are_not_renameable() {
        let src = "flow f\n  # read the config\n  $x = read(\"please read it\")\n  return $x\n";
        let root = root_of(src);
        assert!(
            symbol_at(&root, at(src, "# read") + 2).is_none(),
            "a word inside a comment is not a symbol"
        );
        assert!(
            symbol_at(&root, at(src, "please read it") + 8).is_none(),
            "a word inside a string literal is not a symbol"
        );
        assert!(
            symbol_at(&root, src.find("= read").unwrap()).is_none(),
            "the `=` operator is not a symbol"
        );
    }

    #[test]
    fn a_new_name_must_be_spellable() {
        let src = "flow f\n  $x = 1\n  return $x\n";
        let root = root_of(src);
        let symbol = symbol_at(&root, at(src, "$x = 1")).expect("resolves");
        assert!(valid_new_name(&symbol, "total"));
        assert!(valid_new_name(&symbol, "$total"), "the sigil is optional");
        assert!(!valid_new_name(&symbol, "not a name"));
        assert_eq!(replacement_for(&symbol, "total"), "$total");
    }

    #[test]
    fn go_to_definition_resolves_var_use_to_its_bind() {
        let src = "flow f\n  $x = 1\n  $y = $x\n  return $y\n";
        let root = root_of(src);
        let def = definition_at(&root, src.find("$y = $x").unwrap() + 6).expect("resolves");
        assert_eq!(usize::from(def.start()), src.find("$x = 1").unwrap());
    }

    #[test]
    fn go_to_definition_resolves_op_reference_to_declaration() {
        let src =
            "op greet(name: String) -> String\n  return $name\n\nflow run\n  return greet(\"x\")\n";
        let root = root_of(src);
        let def = definition_at(&root, at(src, "greet(\"x\")")).expect("resolves");
        assert_eq!(usize::from(def.start()), src.find("greet").unwrap());
    }
}
