---
id: D-56
title: FlowClient::analyze with pre-bound seed names — finish the D-01 seam
pillar: Agent
status: done
epic: consumer-gaps
note: "from the 2026-07-06 downstream-consumer review: analyze hardcodes the empty prebound set, so execute_with seeding can't analyze clean unless seeds are declared flow params — the consumer works around it with ~60 lines of Bind-node AST surgery"
---

# FlowClient::analyze with pre-bound seed names

## Goal
Let an SDK consumer analyze a stored flow **as it will actually run** — with the names it intends to
seed via `execute_with` counted as bound — so per-invocation value injection (D-01) no longer forces
either flow-param declarations or hand-prepended `Bind` nodes.

## Why (evidence)
`FlowClient::analyze` passes a hardcoded empty prebound set to `analyze_flow(ast, &ops, &prebound)`
(`crates/flux-sdk/src/flow.rs:334-338`) even though the underlying analyzer takes one (L-15
machinery). `execute_with` (flow.rs:374-404) seeds symbols at run time, so a seed-only name analyzes
as unbound. The reviewed SDK consumer therefore clones the AST and prepends placeholder
`Node::Bind{Lit}` nodes — twice, once to pass analyze and once for real values at execute — ~60
lines of AST surgery the SDK was supposed to absorb.

## Acceptance
- [x] Additive `FlowClient::analyze_seeded(&self, ast, seed_names)` (exact name/signature may be
      refined at impl) that passes the names through as the prebound set; failing-first test: a flow
      referencing `$settings` without declaring it analyzes **unbound** via `analyze` and **clean**
      via `analyze_seeded(["settings"])`.
- [x] `optimize` gets the same seeded variant if trivial (it hardcodes the empty set too,
      flow.rs:~410); otherwise scoped out in Notes.
- [x] Doc cross-links: `execute_with` names `analyze_seeded` as its analysis partner; `analyze`'s
      L-15 caveat paragraph points at the seeded variant.
- [x] Full gate green (build/test/clippy -D warnings/fmt both workspaces); consumer-compat:
      `cargo check` in the downstream consumer workspace unaffected (purely additive — no existing
      signature changed).

## Progress
- 2026-07-06 filed from the consumer review; implementation started same day.
- 2026-07-06 implemented: `FlowClient::analyze_seeded(&self, ast: &DraftAst, seed_names: impl
  IntoIterator<Item = impl Into<String>>)` passes the caller's names through as the prebound set to
  `analyze_flow` (matches the SDK's existing `impl IntoIterator<Item = impl Into<String>>` style,
  see `flux_lang::dsl::WrapBuilder::with_tools`). `optimize_seeded` added as a trivial mirror over
  `analyze::lower` — no non-obvious issues surfaced, so it shipped alongside rather than being
  scoped out. `analyze`'s L-15 caveat and `execute_with`'s doc now cross-link `analyze_seeded`.
  Failing-first test written first and confirmed to fail for the right reason (unbound `$settings`)
  before the fix landed, then re-confirmed green after. Three new tests in
  `crates/flux-sdk/src/flow.rs` under `mod tests`: `analyze_seeded_accepts_an_undeclared_execute_with_seed`
  (unbound via `analyze`, clean via `analyze_seeded`, and actually executes via `execute_with`
  seeding the same name), `analyze_seeded_with_unreferenced_name_is_harmless`, and
  `analyze_seeded_with_empty_set_matches_analyze`. Full gate green: `cargo build --workspace`,
  `cargo test --workspace` (0 failures across all crates), `cargo clippy --workspace --all-targets
  -- -D warnings` (clean), `cargo fmt --check` and `(cd plugins && cargo fmt --check)` (both clean).
  Purely additive — no existing public signature changed, so the downstream consumer's
  `cargo check` is unaffected.

## Notes
- Adoption story filed in the consumer's own repo once landed (replace the double Bind-prepend with
  `analyze_seeded` + `execute_with`).
