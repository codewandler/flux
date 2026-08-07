---
id: C-700
title: "Bound the wire-supplied operation label, and say what Unreachable does not claim"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-server, flux-core]
design: docs/designs/the-substrate-seam.md
note: "C-674 review minors: an authenticated caller's operation string reaches the daemon's stderr unfiltered, and the Unreachable prose reads as `it never got there` when the code deliberately does not claim that"
---

# Bound the wire-supplied operation label, and say what Unreachable does not claim

## Goal

Two small honesty defects from C-674's review, both on the serving side of the remote protocol.

`WireHttpRequest.operation` is copied verbatim into the served request with no length or
control-character bound, and flows to `record_private_admit("web:{operation}", …)` and from there
to the daemon's stderr audit line. An authenticated caller can therefore embed newlines or ANSI
escapes in the serving operator's terminal. `port::bounded_admit_label` exists for exactly this
class and is applied only on the response path. This is log integrity on the serving host rather
than credential exposure, but the daemon's audit line is the operator's evidence and it should not
be forgeable by the caller it is recording.

Separately, the framed route's comment says a broken link leaves the request "in the same
'unknown' position the port's `Unreachable` already describes", while `flux-core`'s error
documentation defines `Unreachable` ("no answer arrived") and `Unknown` ("accepted but cannot
prove its terminal outcome") as distinct. The code picks the weaker, safer classification and is
right to; only the prose overreaches, and the operator-facing prefix "the remote guarded delegate
is unreachable" reads as "it never got there" to someone deciding whether to retry a POST.

## Acceptance

- [ ] Every wire-supplied label that reaches an audit sink or an operator-visible line is bounded
      and control-character-stripped on the serving side, through the existing helper rather than a
      second one; a test drives newlines and escapes through the frame and asserts the rendered
      line is intact.
- [ ] The framed route's comment distinguishes `Unreachable` from `Unknown` accurately, and the
      caller-facing detail for a broken link states that the request may have been sent — so a
      reader deciding whether to retry a non-idempotent effect is not misled.
- [ ] The same detail wording is applied to the generic delegated path, which has the identical
      property and the identical prose problem.
