---
id: C-95
title: "Taint-flow policy through the envelope (epic)"
pillar: Core
status: backlog
epic: taint-flow-policy
design:
note: "EPIC — label byte origins at guarded IO and enforce flow rules; prompt-injection defense becomes a deterministic data-flow gate, not prompt-level pleading"
---

# Taint-flow policy through the envelope (epic)

## Goal
Guarded IO already sees every byte enter (web fetch, plugin frame, file read) and leave (argv,
outbound request, file write). Label origins and enforce flow rules: bytes that arrived from the
web may not reach an argv or a different-host request without an explicit approval naming the
flow. That turns prompt-injection defense from prompt-level pleading into a deterministic
data-flow gate. C-76/C-77 hard-code two specific exfil cases; the generalization is absent from
every spec.

## Acceptance
- [ ] A design doc (`docs/designs/taint-flow-policy.md`) covering: origin labels at each guarded-IO
      entry point, taint propagation through the value store, the flow-rule policy vocabulary, and
      the approval prompt that names the source→sink flow.
- [ ] The epic is broken into implementation stories on the board; each behavioral change ships
      with a failing-first test.
- [ ] Headline proof: web-originated bytes reaching an argv or a different-host request are blocked
      (or forced to a flow-naming approval) by default, pinned by a no-bypass test beside the
      existing envelope tests.

## Progress
- (not started — epic filed from the 2026-07-28 out-of-the-box ideas session)

## Notes
- Generalizes C-76 (http.request $secret exfil) and C-77 (DNS-rebinding pin) from hard-coded cases
  into one deterministic data-flow gate.
- The security story reviewers will cite; everything routes through `Executor::dispatch` and
  `flux-system`, so the chokepoints already exist.
