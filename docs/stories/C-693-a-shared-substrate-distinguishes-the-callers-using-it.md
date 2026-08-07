---
id: C-693
title: "A shared substrate distinguishes the callers using it"
pillar: "Core"
status: backlog
epic: the-substrate-seam
areas: [flux-system, flux-server]
design: docs/designs/the-substrate-seam.md
note: "the agent surface has ServerAuth::Principal with realm-scoped sessions and per-request (Caller, Trust); the remote-system wire has one bearer token and no principal at all"
---

# A shared substrate distinguishes the callers using it

## Goal

The two remoting axes have opposite authority models. The agent surface ships
`ServerAuth::Principal`: every request is authenticated to a principal, sessions are tagged with
and scoped to the caller's realm, and every turn runs under the request principal's
`(Caller, Trust)` rather than the service identity. The substrate surface has none of that —
`flux system serve` takes one bearer token, and `principal` appears nowhere in the remote-system
client or server. Holding the token grants exactly the daemon's authority: its OS identity, its
pinned workspace, its posture. Two engineers sharing one substrate are indistinguishable on it,
whatever the agent side knows about them, and the audit record on the far side cannot say which of
them ran a command.

Today the honest operating rule is *one substrate per authority level* — which is what the
Kubernetes profile's one-writer-per-workspace shape already implies. This story asks whether the
substrate wire should carry a principal at all, and if so what it changes: the daemon's OS
identity is still the ceiling no protocol field can raise.

## Acceptance

- [ ] A design decision records whether the substrate protocol carries caller identity, and what
      it may and may not affect — explicitly stating that the serving process's OS identity, the
      pinned workspace and the posture remain the ceiling regardless of principal.
- [ ] If carried: the far side's audit record names the calling principal for every operation, and
      a per-principal authority (at minimum: refuse) is expressible and deny-by-default.
- [ ] If not carried: the one-substrate-per-authority-level rule is stated in the deployment
      profiles and the host reference, so an operator does not infer isolation the wire cannot
      provide.
- [ ] Either way the documented relationship between an agent-side principal and the substrate it
      selects is explicit — no reader should conclude that agent-side realm scoping constrains what
      the substrate does.
