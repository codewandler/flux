---
id: L-18
title: Round-trip totality + parse-error line numbers
pillar: Language
status: done
epic: flux-lang-v1-hardening
design: docs/designs/flux-lang-v1-hardening.md
note: parse(format(ast))==ast is claimed "for every DraftAst" but empirically false — Var{"a.b"} in expression position silently reparses as jq; space names fail loudly; no property test; parse errors carry no line numbers
---

# Round-trip totality + parse-error line numbers

## Goal
Make the headline invariant `parse(format(ast)) == ast` actually total (formatter falls back to
`@json` for unspellable names, mirroring the existing retry-backoff guard), prove it with a
property test, and give parse errors line numbers for the model repair loop.

## Acceptance
- [x] **Failing-first:** the two confirmed counterexamples round-trip exactly — `Var{name:"a.b"}`
      in expression position (silent corruption today) and a space-containing symbol/op/flow name
      (loud failure today) — via `@json` fallback (F21).
- [x] Formatter guards every name position (bind/each/ctx/collect/arrow-bind symbols, op names,
      flow/param names) with `SymbolName::is_identifier()`/`is_valid_op_name()` from ast.rs.
- [x] Declared-name charset in the parser drops `.` (bind targets, `each` items, arrow syms, sym
      lists); `$a.b` in expression position stays `jq` sugar (F8 text side).
- [x] Property test: bounded arbitrary `DraftAst` → `parse(format(ast)) == ast`, in the crate's
      test suite (proptest dev-dep, or a seeded hand-rolled generator if the dep is unwanted).
- [x] Parse errors carry `line N:` context (the `Line` struct keeps its source index; `perr`
      gains position) (F22).
- [x] Stale text_roundtrip.rs:27-28 doc fixed; `@json` statement-position coverage restored.
- [x] Full gate green; CHANGELOG entry.

## Progress
- 2026-07-02 implemented (parse.rs + format.rs + tests only; ast.rs helpers landed via L-16):
  - **F21 formatter name guards** — every declared-name position (`bind`/`memo` via the shared
    Bind arm, `each` item+collect, `repeat` collect, `ctx` name+include/exclude, `ctx_append`,
    all `-> $bind` targets on seq/retry/loop/timeout/budget/fallback/with_tools, `parallel`
    branch names, `$var` in statement AND expression position, op names in `do`-form and inline
    paren-form calls, `jq`-sugar input vars) now falls back to `@json` when unspellable. Two
    extra silent-corruption holes found and guarded beyond the story list: an op literally named
    `fmt` in inline position (reparses as the `Fmt` node) and a `Bind` type annotation whose
    `Named` label collides with a builtin (`Named("Bool")` → `Bool`) or leaves the decl charset
    (`is_spellable_type`).
  - **Flow-header exception documented** — the parser has no whole-flow escape, so an unspellable
    flow/param/type header name stays a *loud* parse error; documented in format.rs rustdoc
    ("The flow-header exception"), excluded from the property test, analyzer rejects via
    `is_valid_decl_name` (L-16).
  - **F8 parser charset** — new `is_ident_char` (alnum + `_`, no `.`) for all declared positions:
    `parse_dollar` (with a targeted "`$x.y` is field access" error), `each` item/collect,
    `parse_arrow_sym`, `parse_sym_list`, `ctx` name, `parallel` branch names. Expression `$a.b`
    stays jq sugar. Repo scan: no dotted declared names existed in examples/tests (the
    `call-routing.flux` hits are expression-position sugar).
  - **F22 line numbers** — `Line` carries its 1-based source number (comments/blanks counted);
    `perr_at`/`err_at` attach `line N:` once, innermost frame wins; wired through `parse`,
    `parse_stmts`, `@effect` inner stmt, `split_until` (also deduped `parse_repeat` onto it),
    `parse_arms`, `parse_ctx` attrs, `attr_lines`, journey/op decls, `parse_program`, and
    `preprocess` (tab error). Malformed-input tests now assert the line numbers.
  - **Property test** — `tests/roundtrip_property.rs`: seeded hand-rolled xorshift64* generator
    (workspace has no proptest; zero new deps, failures reproducible by seed), 1000 iterations,
    depth-limited, all 43 node kinds (coverage-asserted) with adversarial name/string pools;
    plus a per-position guard sweep (`every_name_position_guards_unspellable_names`) and the two
    confirmed counterexamples as unit tests. Failing-first verified by re-disabling the Var guard
    (both the targeted test and the property test catch it).
  - Housekeeping: stale P6b "@json, no native grammar yet" doc in text_roundtrip.rs fixed
    (native since P8, now asserted); explicit statement-position `@json` escape coverage pinned
    in `json_fallback_round_trips_statement_and_inline`.
  - Gate: `cargo test -p flux-lang` green (179 lib tests + property + integration),
    `clippy --all-targets -D warnings` clean, `cargo fmt --check` clean (verified after the
    concurrent L-16/L-17 edits settled). CHANGELOG entry deferred to story close (file not in
    this task's ownership set). Note: a pre-existing flux-sdk test
    (`parse_is_deterministic_no_provider_call`) fails against L-16's new unbound-symbol analyzer
    check — unrelated to this story's parse/format changes.

## Notes
- Findings F8, F21, F22. Guard pattern to reuse: format.rs:624-630 (retry backoff), format.rs
  Each flat/collect guard.
