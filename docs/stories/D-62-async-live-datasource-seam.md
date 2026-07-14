---
id: D-62
title: Async paged live-backend datasource seam
pillar: Agent
status: done
design: docs/designs/async-live-datasource-seam.md
note: "design-first (2026-07-06 downstream-consumer review): flux's DatasourceBackend is sync + index-shaped — wrong for live paginated APIs; the reviewed consumer built its own paged list/get tool projection"
---

# Async paged live-backend datasource seam

## Goal
Give flux a datasource seam for **live external backends**: async, paged, filterable reads projected
into a uniform two-op tool surface (`<domain>.list {entity,page?,limit?,filters?}` +
`<domain>.get {entity,id}`) — so consumers stop building their own projection layers.

## Why (evidence)
`DatasourceBackend` (`crates/flux-capabilities/src/datasource/mod.rs:51`) is **synchronous** and
**index-shaped** (`upsert`/`clear`/`len`/keyword `search` over local `Record`s) — genuinely the wrong
shape for a live paginated API. The reviewed downstream consumer built the missing layer app-side:
typed pages + entity bindings + two generated ops per domain (`<domain>.list`/`<domain>.get`) with
filter-key validation, limit clamping, and compact id/title/summary row rendering — and documents
the gap in its own module header. Only its per-entity fetch/get closures are app-specific; the
paging/validation/projection machinery is generic.

## Design sketch (to be developed in docs/designs/ before implementation)
- A **second trait** beside the index-shaped one (e.g. `LiveDatasource`: async `list(entity, page,
  filters) -> Page<Row>` + `get(entity, id) -> Option<Row>`), NOT a retrofit of `DatasourceBackend` —
  the two shapes serve different needs (local index vs remote system-of-record).
- Generic tool projection: registering a `LiveDatasource` under a domain name yields the two ops with
  validation + clamping + compact row rendering, honoring evidence-gated surfacing.
- Open questions: filter typing (string-only vs typed), page-token vs offset paging, how rows carry
  references (plugins are references-only — rows must not smuggle secrets/handles).

## Acceptance
- [x] Design doc in `docs/designs/` answering the open questions with the consumer's implementation
      as the reference case.
- [x] Implementation split into D-168 through D-173, including the post-C-62 typed-authority and
      SDK-surfacing requirements.

## Progress
- 2026-07-06 filed (design-first) from the downstream-consumer review.
- 2026-07-15 design accepted. Typed filters, opaque cursors, weak references, exact backend
  authority, evidence-gated surfacing, and the SDK assembly seam are resolved; implementation is
  sequenced as D-168 → D-169 → D-170 → D-171 → D-172 → D-173.
