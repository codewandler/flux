---
id: L-87
title: Find-references and rename over the CST scope model
pillar: Language
status: done
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

- [x] `initialize` advertises `references_provider` and `rename_provider` (with
      `prepare_provider` so clients can pre-validate).
- [x] `textDocument/references` on a `$var` returns its bind site (when
      `include_declaration`) plus every use resolving to *that* binding — never a same-named
      variable in another declaration or another scope.
- [x] `textDocument/references` on a flow/op name returns its declaration and every call site.
- [x] `textDocument/prepareRename` rejects a position that is not a renameable symbol (punctuation,
      a keyword, a literal) instead of returning a bogus range.
- [x] `textDocument/rename` returns a `WorkspaceEdit` covering exactly the reference set, with the
      `$` sigil handled and the new name validated as a legal identifier.
- [x] Failing-first tests: (a) two flows each binding `$x` — renaming one leaves the other
      untouched; (b) an inner shadowing bind renames only the inner scope's uses; (c) renaming a
      composite op updates its declaration and both call sites; (d) the renamed buffer still parses
      clean and lowers to the same shape.

## Progress
- **Done (2026-07-28).** `initialize` now advertises `references_provider` and `rename_provider` with
  `prepare_provider`. `references` resolves through the scope model, so a `$x` in one flow never
  matches a `$x` in another and an inner shadowing bind owns only its own scope; on a flow/op name it
  returns the declaration plus every call site. `prepareRename` refuses punctuation, keywords and
  literals instead of handing back a bogus range, and `rename` validates the new name as a legal
  identifier and handles the `$` sigil.
- **Tests (7):** references stay inside their binding, respect inner shadowing, a composite name
  resolves declaration + call sites, punctuation/prose are not renameable, a new name must be
  spellable, and both go-to-definition paths.


## Notes
- Single-file scope for the rename edit: cross-file rename waits for the workspace catalog in L-89.
  Say so in the story that lands, and keep the `WorkspaceEdit` shape ready for multiple documents.
