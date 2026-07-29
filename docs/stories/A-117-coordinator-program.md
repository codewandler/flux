---
id: A-117
title: The coordinator.flux reference Program + offline end-to-end journey test
pillar: Agent
status: backlog
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [flux-app, flux-channels]
note: "the epic's headline proof — intake → dispatch → sweep → done against MemoryBoard and a stub A2A worker, no credentials, no network"
---

# The coordinator.flux reference Program + offline end-to-end journey test

## Goal
Ship the coordinator itself: a `.flux` Program that declares the board datasource, the webhook /
schedule / a2a / slack channels, the coordinator agent and the intake / dispatch / sweep journeys —
and prove the whole loop runs. This is the story that turns the epic's parts into a thing you can
run with `flux run coordinator.flux --serve`.

Run state is **not** a new store: `fleet.dispatch` writes the `task_id` and `runner` back into the
board `Item`, so the board *is* the run registry, and the `sweep` journey on a `schedule` channel is
cron-driven reconciliation. Crash recovery is then free — restart, sweep, re-derive.

## Acceptance
- [ ] `coordinator.flux` ships as a reference Program: `datasource board`, `channel jira_hook`
      (webhook), `channel nightly` (schedule), `channel inbox` (a2a), `channel ops` (slack),
      `agent coordinator`, triggers, and the `intake` / `dispatch` / `sweep` journeys.
- [ ] Failing-first test: an **offline** end-to-end cycle against `MemoryBoard` and a stub A2A
      worker — an inbound webhook creates an item, `dispatch` claims it and records `task_id` +
      `runner`, the sweep transitions it to `Done` when the stub reports completion. No credentials,
      no network.
- [ ] Failing-first test: **crash recovery** — a fresh `App` over the same board re-derives every
      in-flight item from board state alone and the sweep resumes; nothing was held in memory that
      mattered.
- [ ] Failing-first test: a sweep over many in-flight items does **not** block inbound webhook
      intake (the payoff for A-112).
- [ ] The Program is documented on the website's app/channels pages, and any new config keys are
      covered by the existing config-completeness assertions.

## Progress
- (not started)

## Notes
- Design: [fleet-coordinator.md §5, §8](../designs/fleet-coordinator.md).
- Depends on A-112 (concurrency), A-113 (+ at least one real backend, A-114 or A-115), and A-116
  (dispatch).
