---
id: A-114
title: MarkdownBoard — file-per-item with a derived index, IO via flux-system
pillar: Agent
status: backlog
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [flux-capabilities]
note: "the track-style backend flux already dogfoods; write contention resolved structurally (file-per-item + atomic rename), never a lock"
---

# MarkdownBoard — file-per-item with a derived index, IO via flux-system

## Goal
A zero-dependency, diffable, reviewable `WorkBoard` backend: one markdown file per work item with
frontmatter, plus a generated index — the `docs/stories` + `/track:board` pattern flux already
dogfoods. It is what makes the coordinator usable with no Jira at all, and what makes the board's
history reviewable in git.

## Acceptance
- [ ] `MarkdownBoard` implements `WorkBoard` and **passes the shared contract suite from A-113
      unmodified**.
- [ ] All IO goes through `flux_system::Workspace` — no direct `std::fs` on the backend path.
- [ ] Failing-first test: two concurrent `claim` calls on the **same** item resolve to exactly one
      winner (compare-and-set on the item file), and the loser gets a conflict error, not a
      clobbered file.
- [ ] Failing-first test: two concurrent writes to **different** items never contend — no shared
      mutable file on the write path — and the index is regenerated on read, so a stale or missing
      index is never authoritative and never loses an item.
- [ ] Item writes are atomic (write-then-rename); an interrupted write leaves either the old item or
      the new one, never a truncated file.
- [ ] The board root is configurable and may differ from the coordinator's cwd — a `System`
      construction detail, resolved without any `WorkspaceContext` change.

## Progress
- (not started)

## Notes
- Design: [fleet-coordinator.md §3, §7](../designs/fleet-coordinator.md). The multi-root question
  folds in here rather than being filed separately: remote A2A workers own their own workspace
  pinning, so the only residue is where this backend's files live.
- Depends on A-113 (port + contract suite).
