---
id: C-561
title: "A failed Fleet worker can resume its exact durable turn"
pillar: Core
status: ready
priority: 2
epic: fleet-loop
design: docs/designs/native-board-fleet-cli.md
areas: [flux-cli, flux-runtime]
depends_on: [C-565]
note: "dogfood stop-line — resume currently routes through the availability check that rejects failed workers"
---

# A failed Fleet worker can resume its exact durable turn

## Goal

Make `flux fleet resume WORKER` recover the transient failure state it names, using the same admitted
identity, assignment and durable session without editing Fleet state by hand.

## Acceptance

- [ ] Failing first, a hermetic worker emits malformed or oversized stream output, is recorded
      `failed`, and `resume WORKER` currently refuses that exact state as “not available for
      delivery.” The fixed path transitions through an explicit recovery state and completes a
      bounded follow-up in the same runtime session.
- [ ] Resume preserves worker id, BoardRef, branch, worktree, capability/mode/fence ceiling and
      session. It cannot revive a cancelled or parked worker and cannot create a second writer.
- [ ] Accepted intake is routed only to its addressed target and is completed exactly once. Resuming
      one worker must not drain unrelated main/worker messages into that worker.
- [ ] A process that died after durable `working` delivery is reconciled to an inspectable
      interrupted/failed state before retry; status never claims a live worker solely from stale
      state.
- [ ] Cancellation races, stale revisions, duplicate resume keys and a second recovery failure are
      deterministic and retain the original error plus the new attempt evidence.
- [ ] Focused lifecycle tests and the native Fleet dogfood journey recover without direct edits to
      `.flux/fleet/state.json` or terminal/tmux IPC.

## Notes

- Filed from the C-560 dogfood recovery at Fleet revision 154 on 2026-08-05.
