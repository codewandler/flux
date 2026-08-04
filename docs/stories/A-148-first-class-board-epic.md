---
id: A-148
title: "The first-class board — one fixed tool surface, pluggable backends (epic)"
pillar: Agent
status: backlog
epic: first-class-board
design: docs/designs/first-class-board.md
areas: [flux-lang, flux-datasource, flux-capabilities, flux-sdk]
note: "EPIC — the board leaves the datasource vocabulary per Decision 0006: its own `board` declaration, its own `board:` subject namespace, its own SDK seam; the 11-op WorkBoard surface stays identical across every backend"
---

# The first-class board — one fixed tool surface, pluggable backends

## Goal

Make the work board a first-class Flux concept instead of a `kind "board:…"` prefix hack inside the
`datasource` declaration. Flux-roadmap Decision 0006 removed boards from the datasource vocabulary —
a datasource is read-only by definition, and the board mutates — so the board gets its own Flux-Lang
`board <name>` declaration, its own `board:<domain>/item/<id>` subject namespace, and its own SDK
seam, while the deliberately model-shaped contract stays fixed: one small 11-operation tool surface
with a closed state machine that is identical regardless of backend.

## Acceptance

- [ ] Flux-Lang has a first-class `board <name>` declaration and the `kind "board:*"` datasource
      spelling is retired with a documented migration (L-130).
- [ ] The SDK exposes a board seam with the same all-in-one guarantees as
      `try_with_live_datasource` (A-134, absorbed into this epic).
- [ ] The 11-operation WorkBoard tool surface and its closed state machine are pinned as
      backend-independent: the shared contract suite passes unmodified for every backend, and the
      operation set is the same for memory, markdown, and any future vendor backend.
- [ ] Vendor tracker backends are chartered through the Decision 0006 declared-surface pattern —
      connector-declared status↔state and per-verb operation mappings, Exchange tenant Board
      bindings, every mutation an admitted granted operation — not through plugins (A-115, A-118
      re-pointed; Milestone 3+).
- [ ] Board authority subjects use the `board:` namespace (grammar work shared with D-251).

## Progress

- (not started — filed 2026-08-04 by C-514 from Decision 0006's "Boards are their own first-class
  surface" section)

## Notes

- Design: [first-class-board.md](../designs/first-class-board.md). Decision source:
  `../flux-roadmap/decisions/0006-datasources-are-declared-read-surfaces.md`.
- What lands near-term is the vocabulary and the Flux-side board split (L-130, A-134). The vendor
  generalization (connector board members, Exchange tenant Board bindings) is named and deliberately
  designed later, with Milestone 3 — A-115 and A-118 hold that charter.
- Shipped today and unchanged by this epic's direction: the `WorkBoard` port, `MemoryBoard`,
  `MarkdownBoard`, and the 11 generated `board.*` operations (A-113, A-114, A-130).
- The fleet-coordinator epic keeps its shipped board stories; only the vendor-backend stories and
  the SDK seam move under this epic.
