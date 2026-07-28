---
id: L-85
title: Cursor-aware, scope-correct completion
pillar: Language
status: done
epic: flux-lsp-round-2
design: docs/designs/flux-lsp-round-2.md
note: completion() never reads the cursor position (main.rs:256-261) — every request returns the union of all ops + all node kinds + all prelude types + every `$` byte-scanned from the buffer (main.rs:343-382, scan_symbols:709), while go-to-definition on the same buffer is scope-correct
---

# Cursor-aware, scope-correct completion

## Goal

Make the completion list depend on where the cursor is and what is actually in scope there, so
authoring a `.flux` file stops being a search through an undifferentiated list of everything the
language and the host can do.

## Why (evidence)

- `completion` (`crates/flux-lsp/src/main.rs:256-261`) destructures
  `params.text_document_position` for the URI only; `.position` is never read.
- `completions(text)` (`main.rs:343-382`) pushes, unconditionally and in this order: every op from
  `signatures_for_document`, every `node_kinds` row, every `prelude_types` row, then every `$var`.
- `scan_symbols` (`main.rs:709-731`) is a byte scan for `$` over the whole buffer — it collects
  variables from other declarations, from string literals, and from positions before their own bind.
- The scope model that would answer this correctly already exists from L-68: `all_var_defs`
  (`main.rs:941`), `Def`/`DefRole` (`main.rs:763-786`), and the shadowing-aware `better_binding`
  (`main.rs:1049`) — used by `definition_at` (`main.rs:1014`) but not by completion.
- The original design specified this: "cursor context from the token at the offset (`$`/`@` sigil,
  statement head, arg position)" — `docs/designs/flux-lsp.md:50`.

## Acceptance

- [x] Completion resolves the CST token at the cursor offset (`token_at`, `main.rs:1001`) and picks
      a context: after `$` → in-scope variables only; after `@` → annotations; at a statement head →
      node-kind keywords + ops; in an argument position → ops, `$vars`, and prelude types.
- [x] `$var` candidates come from the L-68 scope model — only definitions whose scope range covers
      the cursor, with an inner shadowing bind winning over an outer one — not from `scan_symbols`.
- [x] No completion is offered inside a comment or a string literal.
- [x] Op items carry `documentation` (the rendered signature from `render_op`, `main.rs:733`) and a
      snippet `insert_text` with parameter placeholders (`InsertTextFormat::SNIPPET`) instead of the
      bare `format!("{}()", op.name)` at `main.rs:351`.
- [x] Failing-first tests: (a) a two-flow buffer where the cursor sits in flow B does not offer flow
      A's binds; (b) the cursor after `$` offers only variables, no keywords or ops; (c) a `$name`
      appearing only inside a string literal is never offered; (d) the cursor inside a comment
      returns no items.

## Progress
- **Done (2026-07-28).** `completion.rs`: `context_at` resolves the CST token at the cursor offset and
  classifies the position (after `$`, after `@`, statement head, argument position); candidates are
  filtered per context instead of returning the union of everything. `$var` candidates come from the
  L-68 scope model, so only definitions whose scope covers the cursor are offered and an inner bind
  shadows an outer one. Comment and string tokens return nothing. Op items carry the rendered
  signature as `documentation` plus a `SNIPPET` `insert_text` with parameter placeholders.
- **Tests (8):** the four the acceptance named (cross-flow binds, `$`-only variables, a name that
  exists only in a string, cursor in a comment) plus statement-head vs argument-position contexts,
  snippet/documentation shape, and inner-shadowing precedence.


## Notes
- Pairs with L-90: cursor context needs the parse, so land the per-document parse cache first or
  accept a second parse per keystroke.
- Keep the completion list stable/sorted so client-side filtering stays predictable; `signatures_for_document`
  already sorts by name (`main.rs:89`).
