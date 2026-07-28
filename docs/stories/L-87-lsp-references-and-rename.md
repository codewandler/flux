---
id: L-87
title: Find-references and rename over the CST scope model
pillar: Language
status: backlog
epic: flux-lsp-round-2
design: docs/designs/flux-lsp-round-2.md
note: neither `references` nor `rename`/`prepare_rename` is implemented (no handler in the LanguageServer impl, main.rs:181-340) — yet the scope resolution a correct rename needs already shipped with L-68 (all_var_defs:941, better_binding:1049)
---

# Find-references and rename over the CST scope model

## Goal

Give `.flux` authors the two navigation/edit operations that go-to-definition implies: see every use
of a `$var`, param, flow, or op — and rename it correctly, honouring shadowing.

## Why (evidence)

- The `LanguageServer` impl (`crates/flux-lsp/src/main.rs:181-340`) implements `initialize`,
  `initialized`, `shutdown`, `did_open`, `did_change`, `did_close`, `completion`, `hover`,
  `formatting`, `document_symbol`, `goto_definition`, `semantic_tokens_full` — and nothing else.
  `initialize` (`main.rs:188-215`) advertises no `references_provider` and no `rename_provider`.
- Go-to-definition already resolves a use → its definition with inner-scope shadowing
  (`resolve_var`, `main.rs:1029-1047`, `better_binding`, `main.rs:1049-1064`) and a reference → its
  declaration (`resolve_ident`, `main.rs:1066`). References is that relation inverted; rename is
  references plus an edit.
- Semantic tokens already compute the bind/use distinction over the whole token stream
  (`semantic_tokens`, `main.rs:1141-1195`), so the "every occurrence of this symbol" walk exists in
  a second place too.

## Acceptance

- [ ] `initialize` advertises `references_provider` and `rename_provider` (with
      `prepare_provider` so clients can pre-validate).
- [ ] `textDocument/references` on a `$var` returns its bind site (when
      `include_declaration`) plus every use resolving to *that* binding — never a same-named
      variable in another declaration or another scope.
- [ ] `textDocument/references` on a flow/op name returns its declaration and every call site.
- [ ] `textDocument/prepareRename` rejects a position that is not a renameable symbol (punctuation,
      a keyword, a literal) instead of returning a bogus range.
- [ ] `textDocument/rename` returns a `WorkspaceEdit` covering exactly the reference set, with the
      `$` sigil handled and the new name validated as a legal identifier.
- [ ] Failing-first tests: (a) two flows each binding `$x` — renaming one leaves the other
      untouched; (b) an inner shadowing bind renames only the inner scope's uses; (c) renaming a
      composite op updates its declaration and both call sites; (d) the renamed buffer still parses
      clean and lowers to the same shape.

## Progress
- (not started)

## Notes
- Single-file scope for the rename edit: cross-file rename waits for the workspace catalog in L-89.
  Say so in the story that lands, and keep the `WorkspaceEdit` shape ready for multiple documents.
