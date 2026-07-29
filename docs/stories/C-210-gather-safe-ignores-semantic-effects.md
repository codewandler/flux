---
id: C-210
title: "gather_safe never reads semantic_effects, so an op can be pre-approval reachable and still declare a durable write"
pillar: Core
status: ready
priority: 7
epic: security-assurance
design: docs/designs/security-assurance.md
note: "SURFACED BY the C-208 review — web.fetch is the first op that is gather-safe AND self-declares write_db; is_consequence_bearing mirrors gather_safe exactly, and neither classifier looks at semantic_effects"
---

# `gather_safe` never reads `semantic_effects`, so an op can be pre-approval reachable and still declare a durable write

## Goal
C-191 defined `is_consequence_bearing` as the exact negation of `flux-flow`'s `gather_safe`, and that
correspondence is load-bearing: it is what lets a coherence violation stand in for "this op should
not run before a human sees it". Both classifiers read `spec.effects` and `intents`. **Neither reads
`spec.semantic_effects`.**

C-208 made that gap live. `web.fetch` and `web.crawl` gained `Effect::Read` — correctly, they are
retrievals — which makes them gather-safe, so they execute in the pre-approval gather phase
(`crates/flux-flow/src/staged.rs:1344`). When wired with a record sink, which is how `flux-cli` wires
them (`crates/flux-cli/src/execution.rs:557-559`), each HTML response upserts a durable `web.page`
datasource record (`crates/flux-web/src/fetch.rs:214-229`). That persistence *is* declared — as the
semantic effect `write_db`, lowering to a `flow.write_db` requirement on `ResourceKind::Datasource`
(`crates/flux-runtime/src/lib.rs:2565-2585`) — through a channel neither classifier consults.

This is **not** an authorization hole and this story should not be read as claiming one. The C-208
review traced the path: `Executor::gate` still evaluates the `flow.write_db` requirement against the
mandatory policy floor on every gather-phase call. The gap is narrower and worth naming precisely:
the classifier that decides *what may run before a human looks* is blind to a field ops use to
declare that they persist state.

## Acceptance
- [ ] The question is answered in writing first, in `docs/designs/security-assurance.md`: should
      `gather_safe` (and therefore `is_consequence_bearing`) take `semantic_effects` into account,
      or is the `Effect`/`intents` pair deliberately the whole contract with `semantic_effects`
      reserved for authorization only? Both are defensible; what is not defensible is the current
      state, where the answer is implicit and nobody has stated it.
- [ ] If the decision is that it should: `gather_safe` and `is_consequence_bearing` move together —
      they must stay exact negations, since C-191's whole design rests on that. A failing-first test
      pins that an op declaring a persisting semantic effect is not gather-safe.
- [ ] If the decision is that it should not: the reasoning is recorded at both seams in code, so the
      next reviewer does not re-file this, and the story closes as "won't do" with that pointer.
- [ ] Either way, the `web.fetch`/`web.crawl` case is covered by a test that states the intended
      behaviour explicitly rather than leaving it as an emergent property.

## Progress
- 2026-07-29 — surfaced by the independent review of C-208, which traced the gather path end to end
  and confirmed authorization is intact. Filed rather than fixed inside C-208, which was already
  scoped to the catalog census.

## Notes
- Seams: `gather_safe` at `crates/flux-flow/src/staged.rs:2447-2475`; `is_consequence_bearing` at
  `crates/flux-spec/src/coherence.rs:135-152`.
- ⚠ `flux-spec` is on the **independent protocol line** (`1.x`). If `is_consequence_bearing` changes,
  that crate needs its own version bump or `scripts/check-crate-versions.sh` reds CI — this has bitten
  twice already. See [[protocol-line-crates-need-own-bump]] in the operator's notes; the mechanical
  check is `./scripts/check-crate-versions.sh` before pushing.
- Related consideration recorded during the C-208 review: treating the record as `write_db` rather
  than `Effect::Write` is itself correct and should NOT be "fixed" by promoting it to `Effect::Write`
  — that would falsely demand a `workspace.write` filesystem resource, which a test at
  `crates/flux-web/src/fetch.rs` deliberately guards against ("the datasource marker must not be
  interpreted as a network destination").
- Sibling of C-208; both descend from C-191's invariant work under the security-assurance epic.
