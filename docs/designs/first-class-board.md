# Design record: first-class board split

**Status:** superseded in part by
[native-board-fleet-cli.md](native-board-fleet-cli.md) and flux-roadmap Decision 0010 ·
**Epic:** [A-148](../stories/A-148-first-class-board-epic.md)

Decision 0006 established the parts that remain binding: a mutable board is not a datasource; it has
its own Flux-Lang declaration, SDK seam and `board:` authority namespace; backends implement a
Flux-owned model contract rather than inventing operations.

The earlier revision of this design also required every board to expose the same eleven operations
and one execution state machine. Decision 0010 supersedes that clause after the product acquired
session, repository-planning and cross-repository purposes. The current contract is a common item
core plus closed general, planning and execution profiles. Scope, profile and backend are independent.
Planning boards also own vision, roadmap, decision and design documents outside the work queue.

The delivered `WorkBoard`, `MemoryBoard`, native MarkdownBoard and eleven generated execution
operations remain the execution profile and retain their contract suite. They are inputs to A-134's
registry rather than a universal shape forced onto every board.

Current implementation and story order live exclusively in
[native-board-fleet-cli.md](native-board-fleet-cli.md). L-130 owns the declaration, A-134 the registry
and SDK seam, C-548 session persistence, C-549 Track compatibility and C-550 federation. Vendor
tracker bindings remain Milestone 3+ and implement a selected profile through the Decision 0006
connector/Exchange ownership pattern.
