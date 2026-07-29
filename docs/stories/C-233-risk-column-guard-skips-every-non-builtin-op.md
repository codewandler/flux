---
id: C-233
title: "The published risk-column drift guard silently skips every non-built-in op, so `fleet.*`, `browser.*`, `web.*` and `consult` are unverified"
pillar: Core
status: backlog
epic: security-assurance
design: docs/designs/security-assurance.md
areas: [flux-tools, flux-cli]
note: "filed from A-131's implementor report — the_published_risk_column_matches_the_registry only checks rows whose op is in try_register_builtins, so a wrong published risk tier on any pack op passes green"
---

# The published risk-column drift guard silently skips every non-built-in op, so `fleet.*`, `browser.*`, `web.*` and `consult` are unverified

## Goal
`the_published_risk_column_matches_the_registry`
(`crates/flux-tools/tests/toolspec_invariants.rs:133`) exists so the Risk column operators read in
`crates/flux-flow/docs/ops-reference.md` cannot drift from the tier the catalog actually enforces.
It only holds for built-ins. The registry it checks against is `try_register_builtins`
(`toolspec_invariants.rs:134-136`), and every reference row whose op is not in it is passed over by

```rust
let Some(tool) = registry.get(op) else {
    continue;
};
```

The skip is documented and was deliberate at the time (`toolspec_invariants.rs:122-131`: rows from
"packs this registry does not assemble (`browser.*`, `web.*`, `consult`)" are "skipped rather than
guessed at"). But it is a *silent* skip — `checked` counts only what matched, so publishing `Low` for
a `Medium` op in any non-built-in pack leaves the gate green. The risk table
(`ops-reference.md:20`) documents the five `browser.*` ops as Medium (`:31-35`), `web.fetch` /
`web.crawl` / `web.search` as Low (`:27-28`, `:48`) and `consult` as Medium (`:42`) — none of those
tiers is verified against a `ToolSpec` by anything.

A-131 makes it concrete: it publishes risk tiers for the `fleet.*` ops into the same table, and those
rows will be skipped on arrival — the story's own Acceptance requires the ops to appear in
`ops-reference.md`, and this guard will confirm nothing about them.

This is the same hole C-208 closed for metadata coherence, and it has the same shape: a gate anchored
to `try_register_builtins` while the thing operators trust is the *production* catalog. C-208's fix
is the template — a census assembled in `flux-cli` (the only crate that can see every pack;
`crates/flux-cli/src/catalog_coherence.rs:20-27` records exactly why the layer map forces that
placement) — so this story should widen the risk check the same way rather than invent a second
mechanism.

## Acceptance
- [ ] The published Risk column is checked against the **production** catalog, not just the built-in
      pack. Reuse C-208's `production_catalog` census in `crates/flux-cli/src/catalog_coherence.rs`
      rather than assembling a second one; the check moves to whichever crate can see the ops, exactly
      as C-208's did.
- [ ] Failing-first test: flip one published tier for a **non-built-in** op in `ops-reference.md`
      (`browser.*`, `web.*` or `consult` — the three the current guard names as skipped) and the gate
      must red. On today's tree it stays green, which is the failure being fixed.
- [ ] The skip stops being silent: a reference row naming an op the census cannot resolve **fails**
      with the op name, rather than `continue`. If some rows legitimately cannot be resolved, they are
      enumerated with a reason — the C-208 `EXCLUDED` pattern (`catalog_coherence.rs:296-300`) — so a
      newly published op cannot inherit an exemption it was never granted.
- [ ] A non-vacuity assertion on the widened check: a floor on rows actually verified, in the spirit
      of the existing `checked` counter and of C-208's `!registry.names().is_empty()` guard. A gate
      that verifies zero rows must not be able to pass.
- [ ] The old built-ins-only assertion is not left behind as a second, weaker copy of the same
      invariant — either it becomes the census-backed one or it is removed with the reason recorded.
- [ ] Standard gate green in both workspaces (root + `plugins/`), `cargo fmt --check` included.

## Progress
- (not started)

## Notes
- Filed 2026-07-29 from the fleet-coordinator integration run, out of **A-131's implementor report**.
  The evidence as given: `the_published_risk_column_matches_the_registry` at
  `crates/flux-tools/tests/toolspec_invariants.rs:123` skips any row in
  `crates/flux-flow/docs/ops-reference.md` whose op is not a built-in, so the published risk tiers for
  the `fleet.*` ops are unverified, and the same hole exists for `browser.*`, `web.*` and `consult`.
  Re-verified against `main` at base `9721daca`; the `#[test]` line is `:133` there (the doc comment
  explaining the skip starts at `:122`).
- The suggested shape is the implementor's: widen this test the way **C-208** widened the coherence
  census. `docs/stories/C-208-full-catalog-toolspec-coherence.md` and
  `docs/designs/security-assurance.md` carry the reasoning for that placement, including why the gate
  cannot live in `flux-tools` (`flux-web` / `flux-eval` / `flux-cognition` sit above it in
  `flux_codegate::layer`).
- Why this is not cosmetic: the Risk tier is what the approval surface shows an operator and what the
  destructive/irreversibility floors key off. A published `Low` on an op the catalog treats as
  `Medium` mis-sets an operator's expectation about a call they are being asked to approve.
- Relates to C-234 (the registration-seam scan only reads `execution.rs`) — same class of bug, a
  drift guard whose coverage is narrower than the thing it is trusted to cover.
