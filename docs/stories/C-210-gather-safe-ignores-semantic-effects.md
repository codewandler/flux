---
id: C-210
title: "gather_safe never reads semantic_effects, so an op can be pre-approval reachable and still declare a durable write"
pillar: Core
status: done
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
- [x] The question is answered in writing first, in `docs/designs/security-assurance.md`: should
      `gather_safe` (and therefore `is_consequence_bearing`) take `semantic_effects` into account,
      or is the `Effect`/`intents` pair deliberately the whole contract with `semantic_effects`
      reserved for authorization only? Both are defensible; what is not defensible is the current
      state, where the answer is implicit and nobody has stated it.
- [x] If the decision is that it should: `gather_safe` and `is_consequence_bearing` move together —
      they must stay exact negations, since C-191's whole design rests on that. A failing-first test
      pins that an op declaring a persisting semantic effect is not gather-safe.
- [~] If the decision is that it should not: the reasoning is recorded at both seams in code, so the
      next reviewer does not re-file this, and the story closes as "won't do" with that pointer.
- [x] Either way, the `web.fetch`/`web.crawl` case is covered by a test that states the intended
      behaviour explicitly rather than leaving it as an emergent property.

## Progress
- 2026-07-29 — surfaced by the independent review of C-208, which traced the gather path end to end
  and confirmed authorization is intact. Filed rather than fixed inside C-208, which was already
  scoped to the catalog census.
- 2026-07-29 — **decided and implemented.** The written answer landed first, in
  [security-assurance.md](../designs/security-assurance.md) § "`semantic_effects` participates in
  gather-safety (C-210)": the blindness is a **defect**, so both classifiers now read the tags. The
  third acceptance branch ("if the decision is that it should not") is therefore not applicable.
  Three facts carried it, none of which the story had: of the consequential tags only `flow.write_db`
  and `model.invoke` clear the default policy floor *without* approval
  (`crates/flux-policy/src/lib.rs:407-446`) — `flow.send_external` is approval-gated,
  `flow.delete`/`flow.money`/`flow.calendar` default-deny; leaning on that would make gather-safety
  depend on a policy file operators are expected to edit; and the `model` case was held only by the
  hand-maintained tier C-208 assigned, which C-208 recorded as an *unenforced* review obligation.
  The story's proposed shape was taken: `FlowEffect::is_consequential()` **derived from `lower()`**
  (consequential iff it lowers to `Effect::Write` or any policy `Action`), so the vocabulary states
  its class once. `Network` is deliberately excluded on the tag channel — the effect-set branch
  already catches unread egress, and classifying it twice would let the two branches disagree.
  Extension is **additive**, so `flux-spec` needed no further bump: it already sits at an unreleased
  1.2.0 (v0.34.0 carries 1.1.0), and a second bump would strand 1.2.0 as a version that never ships.
  `is_consequence_bearing_with_effects` is the complete predicate; `is_consequence_bearing` stays the
  effect-set half. The C-191 correspondence is now **pinned by a test** rather than asserted in prose.
  The product question is answered **yes**: sink-wired `web.fetch`/`web.crawl` become `Risk::Medium`
  and leave the gather path. Cost is one loop round, *not* an approval prompt (`RiskApprover` gates
  writes at `High`+, `dispatch` forces only `Destructive`) — the cost C-208 already accepted for its
  six Group B ops. Exempting the ops, and suppressing the record during gather, were considered and
  rejected with reasons recorded.
  Failing-first verified by neutralizing the three production edits and confirming all four new
  behavioural tests fail, then restoring. Full gate green: `cargo test --workspace` (144 binaries),
  `clippy --workspace --all-targets -D warnings`, `cargo fmt` in both workspaces,
  `scripts/check-crate-versions.sh`, and the regenerated website changelog mirror.

## Notes
- Seams: `gather_safe` at `crates/flux-flow/src/staged.rs:2447-2475`; `is_consequence_bearing` at
  `crates/flux-spec/src/coherence.rs:135-152`.
- **A proposed shape, from C-208's implementor — worth starting from, not binding.** Do *not* simply
  teach both functions to read `semantic_effects`: that widens the same blindness into three call
  sites instead of two, and `semantic_effects` is a `Vec<String>` from a trait hook deliberately kept
  free of the language crate. Better is to make the **tag vocabulary carry its consequence class
  once**, the way `FlowEffect::lower` already maps a tag to `(Option<Effect>, Option<Action>)`.
  `write_db` lowers to `Network` + `flow.write_db` today — a policy action with no host effect, which
  is precisely how it slips past both classifiers. A `FlowEffect::is_consequential()` derived from
  that same lowering, consulted by both `gather_safe` and `is_consequence_bearing`, would close the
  gap in one place and keep the C-191 correspondence both exact *and* complete.
- The product question this story must answer, and cannot dodge: if the gap closes that way,
  **should `web.fetch` stop being gather-safe?** That is a behavioural trade-off (losing pre-approval
  retrieval in the adaptive loop) and not a mechanical consequence — decide it explicitly and record
  it alongside the C-208 posture note in the same design doc.
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
