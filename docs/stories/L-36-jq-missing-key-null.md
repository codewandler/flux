---
id: L-36
title: "jq path on a missing key must yield null (real jq semantics), not a turn-killing fatal error"
pillar: Language
status: done
priority:
epic: flux-lang-evolution
note: "s_362 turn 17535: the finalization plan's `jq(\".transcript\", $x)` on an object lacking the key returned Error::Other → turn_ended{outcome:error} — a fully-gathered turn's evidence discarded; `$a.b` sugar means ordinary field access has the same landmine"
---

# jq on a missing key yields null, not a fatal error

## Goal
`eval_jq_path` (flux-lang runtime, ~:3968) turns a missing key into
`Error::Other("`jq` path: key `…` not found")`, which propagates to a fatal turn end — s_362's one
substantive turn gathered everything, then died at answer synthesis on `.transcript` and discarded
the evidence. Real `jq` yields `null` for a missing path, and `$a.b` is sugar for `jq(".b",$a)`
(ast.rs:49), so ordinary field access carries the same landmine. Change missing-key traversal to
propagate `null` (`.a.b.c` on absent → null). This is a deliberate language-semantics change:
update the reference docs' jq section and the parse/dsl tests that pin the old error.

## Acceptance
- [x] Failing-first test: a plan doing `jq(".transcript", $x)` where `$x` lacks the key binds null
      and the flow completes (today: fatal error). Mirror at the `$a.b` sugar level.
- [x] `jq` tests in parse.rs/dsl.rs updated to the null-on-missing semantics; reference.md's jq
      section documents it (Key invariants if claimed).
- [x] A genuinely malformed path (syntax error) still errors loudly — only MISSING data is null.
- [x] Downstream: nothing in shipped .flux relies on the old fatal (grep assets/ + examples/).

## Progress
- 2026-07-03 filed from s_362 forensics (turn 17535 root cause; the read-only agent verified the
  exact line and the sugar path).
- 2026-07-03 implemented: `eval_jq_path` (flux-lang/src/runtime.rs) now returns `null` for
  traversal through missing data (absent key, or an array index past the end), cascading through
  the rest of the path instead of erroring on the first gap; unmatched `[` and a non-numeric index
  remain hard errors (malformed syntax, not missing data). New failing-first tests in
  `runtime.rs`'s test module: `jq_on_a_missing_key_yields_null_not_a_fatal_error` (direct `Node::Jq`,
  plus chained `.a.b.c` cascade), `dollar_dot_sugar_on_a_missing_field_yields_null_not_a_fatal_error`
  (native-text `$obj.b` sugar via `crate::parse::parse`), and
  `jq_malformed_path_syntax_still_errors` (unmatched `[` / non-numeric index still fatal) — all
  confirmed failing against the old code (`Error::Other("`jq` path: key `b` not found")`) before the
  fix, and green after. Searched `assets/`, `examples/`, and `.flux/` for anything asserting the old
  fatal: none found — the only hits were unrelated mentions of the `jq` node existing (a design note,
  an eval-task tag, and the generated skill/reference tables, none of which claim "errors on
  missing"). Updated `crates/flux-lang/docs/reference.md`'s hand-written `jq` section prose and added
  a "Key invariants" bullet describing the null-on-missing / error-on-malformed split; the
  machine-generated node-kinds/prelude blocks were untouched (no `Node` doc-comment changed) and
  `cargo test -p flux-lang --test skill_in_sync` stays green. No test elsewhere in the workspace
  pinned the old fatal (`cargo test --workspace` green, no ripple fixes needed). Gate green: `cargo
  test -p flux-lang -p flux-tools` (219 + 77 unit tests, all doc/integration tests), `cargo clippy -p
  flux-lang -p flux-tools --all-targets -- -D warnings` clean, `cargo fmt -p flux-lang -p flux-tools
  --check` clean, `cargo test --workspace` green.
