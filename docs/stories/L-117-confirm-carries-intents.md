---
id: L-117
title: "`confirm` approvals carry a real IntentSet"
pillar: Language
status: ready
priority: 12
epic: flux-lang-hardening
design: docs/designs/flux-lang-hardening.md
areas: [flux-lang, flux-flow]
note: "Review F5, MEDIUM — request_approval always sends IntentSet::new(); the host can only policy-check a prose label with a user-supplied risk string"
---

# `confirm` approvals carry a real IntentSet

## Goal

`Node::Confirm` requests approval with an unconditionally empty `IntentSet`
(`runtime.rs:2451-2462`); the only signal the host receives is the free-form label
`"[{risk}] {message}"` where `risk` is an arbitrary string defaulting to `"medium"`. A host that
policy-checks approvals on intents is handed nothing to check. Build the intent set from what the
confirm body will actually do, so approval decisions have machine-checkable content — or, if the
label-only contract is the intended seam, record that decision and document it at the trait.

## Acceptance

- [ ] Failing-first: a `confirm` wrapping an effectful call passes an `IntentSet` naming the
      body's ops/effects to `request_approval`; a bodyless `confirm` passes an explicit
      gate-only marker, not silence.
- [ ] The analyzer's already-gathered effect/op information (`analyze.rs:842-869`) is the source —
      no second effect-derivation path.
- [ ] `risk` is validated against the documented set (`low|medium|high|critical`) at lowering,
      instead of flowing through as arbitrary text.
- [ ] The engine adapter (`flux-flow`'s `ExecutorHost`) consumes the intents, and the OpHost trait
      docs state who enforces what (the crate's default hooks are no-ops — `host.rs:119-131` —
      which stays true but becomes loudly documented).
- [ ] If the decision goes the other way (label-only by design), the design doc records why and
      `host.rs` documents the contract; the empty-set call becomes an explicit named constant.

## Progress
-

## Notes

- Trait-signature caution: `OpHost` is the L0/L3 seam — check what the engine and any protocol-line
  crates re-export before changing `request_approval`'s shape.
- Review: docs/reviews/single/2026-08-01-flux-lang-subsystem-review.md F5.
