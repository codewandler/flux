---
id: D-254
title: "Data pipelines — a pipeline is a Flux-Lang Program (design exploration)"
pillar: Agent
status: backlog
design: docs/designs/data-pipelines.md
areas: [flux-lang, flux-capabilities]
note: "design-exploration, Milestone 3+ — reads + transforms + a declared sink, triggered, installable as an App; no first-class pipeline syntax; the secret-record invariant binds immediately"
---

# Data pipelines — a pipeline is a Flux-Lang Program (design exploration)

## Goal

Explore the pipeline shape Decision 0006 names and deliberately does not design: a **pipeline is a
Flux-Lang Program** — datasource reads, transforms, and a declared sink, triggered by a channel,
event or schedule, and installable as an App. Apps are installed Programs, not a second workflow
model, and pipelines get no first-class syntax for the same reason. This story produces an accepted
design, not an implementation.

## Acceptance

- [ ] The exploration answers what a declared **sink/write-surface contract** looks like for a
      Program that lands records somewhere — as a declaration a Program names, not ad-hoc write ops
      — and how it composes with the Milestone 3 stream and lease vocabulary it is gated on.
- [ ] The **secret-record invariant** is designed as enforcement, not prose: credential-marked
      material never enters a `Record`; anything a connector declares as producing or carrying a
      credential is refused by ingest paths, and ingest runs the redactor. The design names the
      exact seams (ingest helpers, sink contract) where the refusal lives and how a test observes
      it.
- [ ] The 0006 non-goals are restated as scope walls: per-record ACLs (tenant isolation is by
      construction), a query DSL, cross-source joins, caching live rows into the index, and any
      Exchange-side index before Milestone 3.
- [ ] The deferred shapes are named with their gates: Exchange-governed incremental sync into an
      index (indexing remains a separate later binding) and streaming change-feed datasources — both
      Milestone 3+.
- [ ] `docs/designs/data-pipelines.md` moves from exploration to accepted (or is retired with the
      reasoning recorded), and implementation stories are filed from it — none are implied by this
      story.

## Progress

- (not started — the seed design at [data-pipelines.md](../designs/data-pipelines.md) records the
  0006 constraints so exploration starts from the decided boundary)

## Notes

- Filed 2026-08-04 by C-514 from Decision 0006's "Data pipelines" section.
- Explicitly not designed in 0006 and not to be smuggled in here: first-class pipeline syntax
  (rejected — a pipeline is a Program), and any pre-Milestone-3 streaming.
