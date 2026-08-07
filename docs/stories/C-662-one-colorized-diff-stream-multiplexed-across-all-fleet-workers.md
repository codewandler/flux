---
id: C-662
title: "One colorized diff stream multiplexed across all fleet workers"
pillar: "Core"
status: ready
priority: 9
areas: [flux-tui]
epic: fleet-harness-throughput
---

# One colorized diff stream multiplexed across all fleet workers

## Goal

At width, the only way to see what workers are actually changing is to visit each worktree and run
`git diff` by hand. What the operator wants is the opposite shape: **one stream, all workers, file
diffs only, colorized, filterable** — transcript multiplexing, where the unit is a diff hunk rather
than a chat message.

This is not the activity rail and not the transcript. It is a dedicated view whose content is code:
what changed, in which worker, in which file, right now.

## Acceptance

- [ ] One view merges file diffs from every live worker worktree into a single chronological
      stream, labelled by worker and item.
- [ ] Diffs are colorized and rendered as hunks, not raw patch text.
- [ ] Filtering by worker, item, repository and path glob, composable and live.
- [ ] The stream updates as workers write, without re-reading whole worktrees on every tick.
- [ ] Reading is bounded: a worker rewriting a large generated file cannot flood the pane or the
      memory behind it.
- [ ] Fences are respected — a path a worker may not write is never presented as its diff.
