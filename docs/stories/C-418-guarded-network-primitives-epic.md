---
id: C-418
title: "Guarded network primitives — DNS, TCP, UDP and ICMP behind one egress decision (epic)"
pillar: Core
status: backlog
epic: network-primitives
areas: [flux-system, flux-tools]
note: "tracker filed 2026-08-01 by a board audit: five stories (C-284..C-288) carried this slug with nothing stating what the initiative is. ⚠ It overlaps `execution-substrate`'s C-396 and that boundary was never written down — this tracker exists mainly to state it"
---

# Guarded network primitives (epic)

## Goal

Give flux one **egress decision** that every network primitive obeys — name resolution, stream,
datagram and raw — instead of one guard for HTTP and ad-hoc reasoning everywhere else.

Today `flux_system::net::guard_url_scoped`/`guard_url` resolves hostnames to IPs and blocks
private/loopback/link-local/ULA/CGNAT/IPv4-mapped ranges unless the caller holds a scoped grant, and
`AGENTS.md` names it a safety invariant with an explicit "don't hand-roll a second URL guard". That
invariant is about **web egress**. This epic is what it means for everything else.

## Members

| story | status | what it is |
|---|---|---|
| C-284 | backlog | Design guarded network primitives — the shape the rest inherit |
| C-285 | backlog | A guarded DNS operation |
| C-286 | backlog | A guarded TCP operation |
| C-287 | backlog | A guarded UDP operation |
| C-288 | backlog | A guarded ICMP operation |

## ⚠ The boundary with `execution-substrate`, which nobody had written down

[C-396](C-396-datagram-dial-targets.md) ("UDP and ICMP dial targets") sits in a **different epic** and
covers adjacent ground: `net::DialTarget` gaining datagram and raw variants under the existing guard.

The intended layering — **state it, do not assume it**:

- **C-396 is the substrate primitive** — the dial target and its guard integration, in flux-system.
- **C-287 / C-288 are the op surface** — what the model can call, with intent declaration and
  per-reply checking on top.

So C-396 is the floor those two build on, and neither should re-derive the guard inside an op. ⚠ If an
implementer finds the layering does not hold — that the primitive cannot be built without deciding the
op surface, or that C-396 and C-287/C-288 are one piece of work under two names — **that is a backlog
problem to raise, not something to resolve inside a diff**. Two stories describing one job leaves both
half-done.

## Acceptance (for the epic)

- [ ] C-284 lands first and the rest inherit its shape; a second guard is never introduced.
- [ ] Every primitive resolves before it decides, and decides on the **resolved address**, not the
      name — the property `guard_url_scoped` already has and the reason it exists.
- [ ] ⚠ Raw sockets are a **privilege** question as much as an API one. An unheld `CAP_NET_RAW` must
      refuse at construction; C-396's own note states why — a check that happens on the wire has
      already leaked the attempt.
- [ ] Every primitive behaves diagnosably under C-410's fail-closed sandbox, where the network may be
      closed: the failure must name the sandbox rather than look like an unreachable host.
- [ ] The relationship with `execution-substrate` is recorded in `docs/roadmap.md` so the next reader
      does not rediscover it.

## Notes

- Filed as part of the C-406 curation sweep. It is a **tracker**, not new scope: C-284..C-288 already
  existed and already carried this slug.
- The `verified-webhook-channel` epic (C-419) is the inbound counterpart — this one is outbound.

## Progress

- Filed 2026-08-01. Members unchanged; the boundary statement above is the new content.
