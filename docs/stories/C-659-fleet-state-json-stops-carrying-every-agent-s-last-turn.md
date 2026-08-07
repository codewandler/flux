---
id: C-659
title: "Fleet state.json stops carrying every agent's last turn"
pillar: "Core"
status: backlog
epic: fleet-harness-throughput
areas: [flux-cli, flux-orchestrate]
note: "every fleet verb fully parses state.json; last_turn blobs accumulate per agent and nothing prunes them"
---

# Fleet state.json stops carrying every agent's last turn

## Goal

Make the cost of a fleet command a function of *live* work rather than of everything the fleet has
ever done. `.flux/fleet/state.json` accumulates one full recorded model turn per agent and never
sheds any of it, so every verb — including pure reads — pays for the entire lifetime history of the
workspace.

`read_fleet_state` parses the whole document on every `FleetAction`, and every mutation
re-serializes and rewrites it. In an observed workspace that had been running waves for some weeks:

| key | size |
|---|---|
| `agents` | 4.79 MB |
| `waves` | 4.12 MB |
| `idempotency` | 2.06 MB |
| `intake` | 829 KB |
| **total** | **12.9 MB** |

`flux fleet status` took ~8.6 s wall (4.2 s user, 4.7 s system) to print a twelve-line summary. Of
the 4.79 MB `agents` blob, **4.6 MB was `last_turn`** — recorded model turns embedded directly in
lifecycle state, with a single worker record reaching 885 KB. Of 53 registered agents, **zero were
active**: 39 cancelled, 8 handoff-accepted, 3 completed, 3 failed. Every one of them was parsed on
every invocation.

Two things make this a defect rather than a tuning opportunity:

- **There is no GC path.** `reclaim_finished_waves` frees on-disk build output and worktrees through
  `reclaim_wave_storage`, bumps the revision and journals `wave.reclaimed`. It never touches
  `state.agents` and never prunes `last_turn`, so the documented maintenance verb reclaims tens of
  gigabytes of disk while leaving the latency untouched. An operator who runs it and sees no
  speed-up has no remaining supported move.
- **The turn is already durable elsewhere.** `events.ndjson` is the append-only journal. Holding a
  second full copy of each turn inside lifecycle state duplicates the record and puts the larger of
  the two copies on the hot path of every read.

## Acceptance

- [ ] Failing-first test: persist a state carrying several terminal agents whose `last_turn` bodies
      are large, round-trip it through the state writer, and assert the written document holds no
      turn body for a terminal agent. It fails before the change.
- [ ] An agent in a terminal status (`cancelled`, `completed`, `failed`, `handoff-accepted`) keeps
      its bounded identity, status, assignment, commit and handoff fields in `state.json`, and does
      not keep the turn body.
- [ ] The full turn remains recoverable for a terminal agent through the event journal and the
      existing `fleet inspect worker` read. A test asserts the diagnosis path still resolves it, so
      this trades duplication for indirection and not for data loss.
- [ ] The `idempotency` ledger is bounded by an explicit policy rather than growing with every
      mutating call, and the bound is asserted.
- [ ] Existing state written by an older binary loads without migration error and is compacted on
      first write, rather than requiring an operator to delete or hand-edit it.
- [ ] A test pins the intent: the serialized size of a state with N terminal agents does not grow
      with the size of the turns those agents ran.

## Progress

- Not started. Filed from a live diagnosis; see Notes for the measurement method.

## Notes

- Code pointers (`crates/flux-cli/src/board_fleet_cmd.rs`): `read_fleet_state` is the full-parse
  entry point every verb goes through; `reclaim_finished_waves` / `reclaim_wave_storage` is the
  maintenance path that does not prune; `persist_fleet_mutation` is the full rewrite.
- To reproduce the measurement without a long-lived workspace, seed a state with terminal agents
  carrying synthetic turn bodies and time any read verb — the cost is in the parse, not the
  projection, so any verb shows it.
- Sibling in this epic: *Let the coordinator's operator transcript roll over instead of growing
  forever*. That story bounds the transcript; this one bounds lifecycle state. Same failure shape,
  different artifact — worth landing with consistent retention vocabulary.
- Sequencing against [C-657](C-657-split-board-fleet-cmd-so-fleet-verbs-stop-serialising-on-one-file.md):
  both write `board_fleet_cmd.rs`, so they cannot share a wave. C-657 first is the cheaper order.
- Out of scope, worth its own story: a read verb prints nothing until its projection completes, so
  a slow read is indistinguishable from a hang at the terminal. Bounding the state fixes the
  latency; it does not give a long read a progress affordance.
- `FleetAction::Start` has no already-running guard — each call re-reads config, rebuilds the
  coordinator capability manifest, bumps the revision and rewrites the whole document. That is
  convergent by design and is how config edits take effect, but it means a repeated `start`
  invalidates any concurrent `--if-revision` guard and pays the full rewrite each time. Noted here
  as context for the rewrite cost; not part of this story's Acceptance.
