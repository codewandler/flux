---
title: Work boards and the fleet
description: "Continue to the complete AI-assisted development guide for boards, backends, and local Flux sub-agents."
---

# Work boards and the fleet

:::info Availability
Native `flux board` and `flux fleet` landed after v0.55.0. Source installs from current `main`
include them; packaged-release users need v0.56.0 or newer.
:::

The board and fleet documentation has moved into one task-oriented section:

- [AI-assisted development](../coding/overview.md) explains how vision, roadmap, decisions,
  designs, planning stories, local sub-agents, reviews, gates, and explicit apply fit together.
- [Boards](../coding/boards.md) covers session/repository/workspace scopes, general/planning/
  execution profiles, all backends, Track compatibility, planning documents, JSON automation, and
  exact current/history statistics.
- [Fleet and local sub-agents](../coding/fleet.md) covers workspace configuration, scheduling,
  durable handoffs, same-session rework, inspection, restart, final gates, and the no-implicit-
  publication boundary.

For automation, begin with the compact installed-version guides and then inspect the full schema:

```sh
flux board skill
flux fleet skill
flux board schema --output json
flux fleet schema --output json
```

An execution board bound as `board` generates the complete operation set: `board.list`,
`board.get`, `board.create`, `board.transition`, `board.claim`, `board.comment`,
`board.record_dispatch`, `board.query`, `board.comments`, `board.reassign`, and
`board.record_evidence`. New backends implement this board contract; they do not become datasource
kinds.

Each `board.query` row has a stable shape: `id`, `title`, `state`, `assignee`, `runner`, `task_id`,
`depends_on`, `repo`, and `attempts`. Optional values are explicit `null`s rather than omitted
fields, so authored flows can branch on the same schema for every backend.
