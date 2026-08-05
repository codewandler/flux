---
title: Work boards and the fleet
description: "Continue to the complete AI-assisted development guide for boards, backends, and local Flux sub-agents."
---

# Work boards and the fleet

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
