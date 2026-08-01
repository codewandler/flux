---
id: C-437
title: "Which guarantees travel over a remote link — and which quietly become someone else's problem"
pillar: Core
status: ready
priority: 7
design: docs/designs/remote-agents.md
epic: remote-agents
areas: [flux-system, docs]
note: "⚠ the story most likely to be silently downgraded to `mostly the same`. flux's invariants — one egress guard, redaction, default-deny authorization, the OS sandbox at the single spawn choke point — are stated for the NATIVE substrate. C-398 owns the general question; this is its remote instance, and it must produce a table a user can act on"
---

# Say exactly what survives the network

## Goal

A statement a user can act on: which of flux's guarantees hold when the substrate is remote, which
become the remote's responsibility, and which do not apply at all.

## Why it needs its own story

flux's safety invariants are stated for the native substrate: all IO through `flux-system`, egress
through `guard_url_scoped`, secrets redacted from model-visible output and never off the machine,
default-deny authorization, an OS sandbox at the single spawn choke point.

⚠ **Over a remote link, those are three different categories and the difference matters.** Some travel
(authorization is decided by the local runtime before anything is sent). Some become the remote's
(the OS sandbox is whatever the remote applies). Some **change meaning entirely** — *"secrets never off
the machine"* is a claim about a machine, and there are now two.

That last one is the reason this is `ready` ahead of the link itself. It is a property people rely on,
and its meaning shifts the moment a remote exists.

## Acceptance

- [ ] A per-guarantee table: **travels · becomes the remote's · does not apply**, with a one-line reason
      each. ⚠ "Mostly the same" is not a statement — the table is the deliverable.
- [ ] ⚠ **The secret-handling row is worked out explicitly**, because *"never off the machine"* has to be
      re-stated once there are two machines. What crosses the link, what does not, and what an operator
      must assume.
- [ ] Each row cites where the guarantee is stated (`AGENTS.md`, `vision.md`, the relevant module doc)
      so a reader can check rather than trust.
- [ ] ⚠ **Where a guarantee does not travel, the code says so at the boundary** — not only the doc. A
      guarantee that silently stops applying is this repo's recurring defect class, on the surface where
      it would cost most.
- [ ] Consistent with [C-398], which owns the same question for `flux-system` bound without
      `flux-runtime`. ⚠ Extend that statement rather than writing a second, divergent one.
- [ ] Full gate green.

## Notes

- Settleable ahead of [C-436](C-436-flux-tui-remote.md): the analysis needs no link, and doing it after
  the link works means doing it under pressure to conclude that everything is fine.
- Feeds [C-440](C-440-the-topologies-page.md) directly — the public page needs exactly this table, per
  topology.
- ⚠ Related sharp edge: if the remote is already a microVM, flux's own OS sandbox may be redundant,
  doubled, or absent. "Absent because the remote is isolated" must be a stated decision, not an emergent
  one.

## Progress

- Filed 2026-08-01 with the remote-agents epic.
