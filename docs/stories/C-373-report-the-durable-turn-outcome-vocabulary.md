---
id: C-373
title: Report the durable turn outcome vocabulary on the wire
pillar: Agent
status: backlog
epic: serving-surface-and-turn-outcome-residuals
design: docs/designs/serving-surface-and-turn-outcome-residuals.md
note: "turn_end.outcome is a two-valued ok|error projection of a seven-valued durable vocabulary — suspended, max_iter, cancelled and a DENIED APPROVAL all reach an automation client as outcome:ok with exit 0. This is the C-226 failure mode one branch over"
---

# Report the durable turn outcome vocabulary on the wire

## Goal

Let an automation client distinguish "finished" from "parked on an approval", "ran out of
iterations", "was cancelled" and "was denied" — the distinction C-226 established for provider
errors, extended to every terminal state the engine already records.

## Acceptance

- [ ] `turn_end.outcome` carries the durable terminal state, not `if error.is_some() {"error"} else
      {"ok"}` (`crates/flux-cli/src/stream_json.rs:201`); at minimum `suspended` and `max_iter` are
      distinguishable from `ok`.
- [ ] Exit-code semantics are defined for each terminal state and documented for automation clients.
- [ ] A denied approval (`crates/flux-flow/src/loop_host.rs:937-945`) is not reported as a
      successful turn — nothing executed.
- [ ] Failing-first: NDJSON-level tests driving a suspended turn, a max-iteration turn, a cancelled
      turn and a denied-approval turn, each asserting a non-`ok` outcome and its exit code.
- [ ] The A2A mapping stays consistent — durable `error` already maps to `TaskState::Failed`
      (`crates/flux-server/src/a2a.rs:1298`); the new states get an explicit mapping too.

## Progress

- 2026-08-01 — filed from validation of OUTCOME-01. The reviewed defect is fixed; this is the same
  class on the branches the fix did not cover.

## Notes

- Changing `turn_end.outcome`'s value set is a protocol change for stream-json consumers — it needs
  a WHATS-NEW entry and a version decision.
