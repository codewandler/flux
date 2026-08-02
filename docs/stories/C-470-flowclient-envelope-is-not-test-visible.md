---
id: C-470
title: "`FlowClient`'s resolved envelope is not test-visible, so the flow door's posture is unproven"
pillar: Core
status: ready
priority: 4
areas: [flux-sdk]
note: "spun out of C-444: the confinement raise is asserted on the Client door only. Both doors share one Envelope, which is why they agree — but nothing test-visible proves the flow door resolves the same posture, and that is the door an SDK embedder is most likely to use"
---

# Two doors, one envelope, one test

## Goal

Make `FlowClient`'s resolved safety posture observable to a test, so the guarantee C-444 establishes on
one SDK door is *proven* on both rather than inferred from shared construction.

## The finding

[C-444](C-444-sdk-secure-defaults.md) makes an autonomous posture carry its own confinement: blanket
`auto_approve` with no injected `Approver` resolves the sandbox to `require`
(`crates/flux-sdk/src/envelope.rs`, `resolve_sandbox`). Its tests
(`crates/flux-sdk/tests/secure_defaults.rs`) assert this on the `Client` door.

`FlowClient` has no public `system()` or `resource_limits()` accessor, so **the same assertion cannot be
written against the flow door.** The two doors do share one `Envelope`, and that shared construction is
the actual reason they agree — this is a testability gap, not a known divergence. But the distinction
matters: today the flow door's posture holds because of a structural fact no test observes, so a future
refactor that gives `FlowClient` its own resolution path would break confinement on that door with a
green suite.

⚠ **`FlowClient` is plausibly the door an SDK embedder reaches for first**, since flows are the SDK's
headline surface. Leaving the unproven door as the more-used one is the wrong way round.

## Acceptance

- [ ] A failing-first test that observes `FlowClient`'s **resolved** posture — not its inputs — and pins
      that an auto-approving `FlowClient` is confined and bounded exactly as the `Client` door is. It
      must fail if `FlowClient` is given an independent resolution path.
- [ ] Whatever accessor makes that possible is a deliberate public-API addition on
      `codewandler-flux-sdk`, documented, and carries its version decision. ⚠ Prefer the narrowest
      surface that makes the property observable — an accessor exposing the whole `System` to read one
      posture is a larger commitment than the test needs, and a published crate cannot un-expose it.
- [ ] ⚠ A test that pins the two doors resolve the **same** posture from the same inputs, so the
      agreement is asserted rather than structural. That is the actual deliverable; a second copy of the
      client-door test is not.
- [ ] No behaviour change: this story makes an existing guarantee observable and must not alter what
      either door resolves.

## Notes

- Disclosed by C-444's implementor as ADJACENT 1, deliberately not fixed there: adding the accessor would
  have widened that story's public-API surface and voided its failing-first proof. Correct call.
- ⚠ An alternative worth considering before adding public API: if the resolution can be pinned from
  *inside* the crate (a `#[cfg(test)]`-visible or `pub(crate)` observation, or a unit test in
  `envelope.rs` covering both construction paths), that closes the real risk with no published-surface
  commitment at all. Try that first.
- Related: [C-444](C-444-sdk-secure-defaults.md), [C-463](C-463-autonomy-postures.md).
- Filed 2026-08-02 out of C-444's handoff.
