---
id: C-714
title: "The served router's timeout and a request's budget agree"
pillar: "Core"
status: ready
epic: first-class-hosts
areas: [flux-server, flux-web]
design: docs/designs/the-substrate-seam.md
note: "C-701's review: the router times out at 120s while a caller's budget clamps at 300s, and the 408 that results is classified Refused rather than as a timeout the caller can recognise"
priority: 4
---

# The served router's timeout and a request's budget agree

## Goal

Two ceilings disagree, and the one that fires first is invisible. `REQUEST_TIMEOUT` is 120 seconds
applied as a `TimeoutLayer` over the whole served router, returning 408; both web operations clamp
a caller's budget to `MAX_TIMEOUT_SECS` of 300. So a framed request whose budget exceeds two
minutes has always become a 408 — and because 408 is neither success nor `BAD_REQUEST`, the client
maps it to `Refused("remote-system HTTP status 408 …")` rather than to a timeout the caller can
recognise and act on.

C-701 makes the window materially easier to reach: a retry chain can now spend up to 60 seconds
waiting *inside* the served request, on top of the attempts themselves. Nothing is unsound — the
request fails closed — but an operator debugging it sees a refusal that names a status code rather
than "this took longer than the daemon allows."

## Acceptance

- [ ] The two ceilings are reconciled deliberately: either the served router's timeout accommodates
      the budget a caller may legitimately ask for, or a framed request's budget is clamped to what
      the router will allow, and the choice is stated where both constants live.
- [ ] A request that exceeds whichever ceiling survives is reported as a timeout the caller can
      distinguish from a refusal, carrying which side timed out.
- [ ] Retry waits are accounted against the same ceiling as the attempts, so a chain cannot pass
      the router's limit through waiting alone.
- [ ] A test drives a request past the ceiling and asserts the classification, not just the failure.
