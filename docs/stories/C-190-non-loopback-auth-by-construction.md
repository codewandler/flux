---
id: C-190
title: "Make the non-loopback auth invariant hold by construction, not only inside serve_on"
pillar: Core
status: ready
priority: 7
epic: security-assurance
design: docs/designs/security-assurance.md
note: "REVIEW — AGENTS.md says 'there are no bypass paths, don't add one'; the unauthenticated-non-loopback refusal lives in serve_on (lib.rs:457) so a caller mounting the router directly gets no guard at all"
---

# Make the non-loopback auth invariant hold by construction, not only inside serve_on

## Goal
`serve_on` refuses to bind an unauthenticated listener to a non-loopback address — a genuinely good
safeguard for a daemon that can execute operations. But the check lives in the serving function, not
in the router, so a lower-level caller that mounts the router into their own server inherits none of
it and is silently responsible for re-deriving the invariant. `AGENTS.md` is explicit that there are
no bypass paths; this is one that already exists. Make the invariant structural.

## Acceptance
- [ ] The unauthenticated + non-loopback combination is unrepresentable or refused at **router
      construction**, not only at `serve_on`. Options to weigh in the design: a bind-address-aware
      constructor, or an explicit opt-out type (e.g. `UnauthenticatedLoopbackOnly`) a caller must
      name to get the open router.
- [ ] Failing-first test: a caller that mounts the router directly and serves it on a non-loopback
      address cannot reach a protected route unauthenticated. This test must fail against the
      current tree — that failure is the proof the gap is real.
- [ ] `ServerAuth::Open`'s doc comments (`lib.rs:49,:56,:70,:90`) are updated to describe the
      enforced guarantee rather than the caller's responsibility.
- [ ] Any intentional escape hatch is explicit, named, and logged loudly at startup.

## Progress
- (not started)

## Notes
- Verified: `crates/flux-server/src/lib.rs:457` — `"refusing unauthenticated non-loopback bind on
  {addr}; set FLUX_SERVER_TOKEN or bind …"`. The refusal is real and correct; its **placement** is
  the finding. `lib.rs:438` confirms it is `serve_on` that requires the loopback bind for
  `ServerAuth::Open`.
- Ranked above the review's own ordering for this one: the reviewer mentions it as a passing caveat
  under "server hardening", but it is not a hardening nice-to-have — it touches envelope integrity,
  which is flux's central claim.
- Related but distinct: `a2a.rs:1575` admits "only the three loopback spellings" for its own target
  check. Worth confirming both paths agree on what loopback means rather than each deciding
  separately.
- Source: [2026-07-29 review](../../reviews/2026-07-29-security-posture-desk-review.md), the
  "Lower-level users can also mount the router directly" caveat — verified.
