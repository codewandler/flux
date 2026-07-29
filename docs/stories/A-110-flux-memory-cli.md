---
id: A-110
title: "flux memory list/show/forget — the inspect surface and the --stale review queue"
pillar: Agent
status: backlog
epic: evidence-pinned-memory
design: docs/designs/evidence-pinned-memory.md
note: "`--stale` is the maintenance loop — the review queue for knowledge whose evidence moved; flux never silently forgets on the agent's behalf, pruning is a user verb"
---

# flux memory list/show/forget — the inspect surface and the --stale review queue

## Goal
Make memory auditable and prunable by the human. Memory the user cannot see is exactly the vibes
scratchpad this epic exists to avoid, so the inspect surface is part of the feature, not a follow-up.

## Acceptance
- [ ] `flux memory list [--scope project|global] [--stale]` lists entries with claim, age, and
      freshness.
- [ ] `flux memory show <id>` prints the claim, the **full** citation (stream, event id, turn,
      SHA, pinned paths), and — when stale — what changed since the pinned SHA.
- [ ] `flux memory forget <id>` appends a tombstone (A-107); the entry leaves the projection and its
      history stays in the log. **Failing-first test**: after forget, the entry is absent from
      `list` and from injection, and present in the raw stream.
- [ ] `--stale` is the review queue: it lists exactly the entries A-109 would mark stale, using the
      same computation — asserted, so the CLI and the injector cannot drift.
- [ ] Explicit subcommands only, no implicit default-run (the project's CLI convention).
- [ ] `list`/`show` are pure reads: a test asserts the store's event count is unchanged and no
      provider is constructed (mirrors the C-132 export posture).
- [ ] Full gate green.

## Progress
- Not started.

## Notes
- Design: [evidence-pinned-memory.md](../designs/evidence-pinned-memory.md).
- Blocked by A-107; `--stale` additionally needs A-109's staleness computation, which should be
  extracted as a shared function rather than duplicated here — that shared seam is what the
  "cannot drift" acceptance criterion pins.
