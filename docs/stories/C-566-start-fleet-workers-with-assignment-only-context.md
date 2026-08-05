---
id: C-566
title: "Every Fleet story worker starts with assignment-only context"
pillar: Core
status: ready
priority: 0
epic: fleet-loop
design: docs/designs/native-board-fleet-cli.md
areas: [flux-cli, flux-runtime]
depends_on: [C-560]
note: "dogfood stop-line — fresh writers must not inherit the main conversation, global goal dump or another worker session"
---

# Every Fleet story worker starts with assignment-only context

## Goal

Start every story writer as a fresh, assignment-scoped workhorse whose context contains only its
worker contract, exact Board item and pinned worktree assignment. Coordinator conversation,
exploration history and unrelated Fleet state remain outside the worker's model context.

## Acceptance

- [ ] Failing first, a hermetic two-worker fixture records distinct launch arguments and proves the
      current first-turn prompt leaks the Fleet-wide revisioned goal set into both workers.
- [ ] A first story-worker turn uses a new worker-specific store and no continuation flag. Its
      prompt contains the configured worker instructions plus exact BoardRef, branch, worktree and
      pinned base, but no main session, intake text, global goal dump, other worker identity or
      other story assignment.
- [ ] The worker is explicitly a writer for the assigned contract: it must inspect the owning
      repository's `AGENTS.md`, story and linked design, create the requested implementation and
      evidence, and must not select, observe or explore unrelated work.
- [ ] Only an acknowledged message or rework addressed to that exact worker may continue its
      durable session. Main turns, maintenance tasks and a different worker never select that store
      or add context to it.
- [ ] Durable status and turn receipts expose a bounded context-origin manifest (worker contract,
      BoardRef and assignment revision/digest) without persisting prompt bodies or conversation
      content.
- [ ] Focused lifecycle tests cover initial launch, parallel workers, same-worker continuation,
      restart and cancelled/failed workers without a provider credential or network.

## Notes

- Filed from the first five-writer native Fleet launch review on 2026-08-05. Session-store isolation
  already exists; Fleet-wide goal injection is the observed prompt-isolation defect.
