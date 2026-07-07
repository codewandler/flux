---
id: A-45
title: Hermetic replay engine + `flux replay` — re-execute a past run, offline, model-free
pillar: Agent
status: done
design: docs/designs/time-machine.md
epic: time-machine
note: "Time Machine Phase 1 SHIPPED 2026-07-07 — `flux replay <session|last>` re-executes any recorded run offline (plans from plan_source, ops from tape, ~400µs vs 78ms live in the smoke); divergence = loud exit 1"
---

# Hermetic replay engine + `flux replay`

## Goal
Re-execute any recorded run exactly — offline, with no model call and no live IO, side-effects served
from the C-43 cassette instead of re-firing. This is the slice that makes the Time Machine real:
ship it and "replay a run" works end to end.

## Acceptance
- [ ] `CassetteHost(Replay)` serves recorded cells without touching the inner host (side effects
      never fire) via the **out-of-order-tolerant matcher**: scan forward from the cursor for the
      first unconsumed cell matching `(op, input_hash OR input_hash_redacted)` — strict-sequential
      for serial plans, absorbs nondeterministic `parallel`-branch interleavings
      (`try_join_all`, `runtime.rs:1477`), and matches redaction-shifted inputs downstream of a
      `redacted` cell. No matching unconsumed cell → hard `ReplayDiverged{at}`, never silent.
- [ ] Failing-first test for each matcher property: a two-branch `parallel` flow recorded once
      replays green regardless of recorded interleaving; a secret-bearing flow (output→input
      dataflow through a redacted value) replays green via the redacted hash.
- [ ] Replay engine (`crates/flux-flow/src/replay.rs`, L3): read `run_trace`+`turns`, and execute
      each accepted plan **in recorded order, reproducing the loop host's recorded dispositions** —
      an accepted plan followed by no statements/cells was the A-05 identical-plan skip and must not
      re-execute; halted plans replay only their completed prefix (recorded `ResumeLedger`). Each
      `plan_source` parses via `FlowClient::parse` (no model) and runs under `CassetteHost(Replay)`
      over one fresh in-memory store; cross-turn symbol state persists in that store.
- [ ] `flux replay <SESSION|last> [--turn N] [--sub-agents] [-o json|pretty]` (non-agent subcommand,
      sibling of `Sessions`/`Usage` at `crates/flux-cli/src/main.rs:202`); `resolve_run` generalizes
      `resolve_resume_session` (`main.rs:2477`). Renders via the existing
      `CliSink`/`style_marked_plan`/`format_evidence`/`risk_badge` — transcript pixel-identical to the
      original minus latency.
- [ ] Sub-agent children replay recursively via a new `EventStore::children_of(session)` projection
      (`SELECT n FROM streams WHERE correlation_id=?`).
- [ ] Failing-first tests: (1) **hermetic determinism** — record a pure flow, drop the value store,
      `replay_session` with provider=None → replayed trace + bound symbols + return value equal the
      recorded ones (`flux-flow/tests/replay_determinism.rs`); (2) **model-free full-run replay** —
      record a `-m mock` turn, `flux replay` with NO provider configured → same answer, no provider
      constructed; (3) **divergence surfaces loudly** — corrupt one cell's `input_hash` →
      `ReplayDiverged{at}`.
- [ ] Full gate green; layering intact.

## Progress
- 2026-07-07 DONE. `crates/flux-flow/src/replay.rs` (driver) + `flux replay` (non-agent
  subcommand). Notes beyond the acceptance:
  - The execution list derives from the trace's `StatementCompleted`/`PlanHalted` rows
    (`flow_key` + node-reset segmentation), reproducing loop-host dispositions by construction;
    for recordings with NO statement rows (the non-resumable `flow run` path ledgers nothing) the
    driver falls back to accepted-plan order — found via smoke, fixed same day (a no-op "success"
    replay was the failure mode).
  - Offline guarantee verified live twice: a mock agent turn AND a `flow run` recording each
    replayed transcript-identical (~400µs vs 78ms live) with the deleted artifact NOT recreated;
    the driver runs over the lazy provider (never constructed) and a scratch store (replay never
    writes to the real log).
  - Tests: `replay_session_reproduces_a_recorded_turn_hermetically` (canned responses exhausted —
    any planner call would fail loudly), `replay_session_absorbs_parallel_branch_interleaving`
    (real recorded `parallel` run), `replay_session_surfaces_divergence_loudly` (hand-built
    session with mismatched plan_source → `no matching unconsumed recorded cell`). Full gate
    green.
  - `--turn N` filters executions to that turn's accepted plans (cross-turn symbol refs then fail
    honestly); `--sub-agents` recurses `EventStore::children_of` (new, the A-08 linkage), child
    errors reported without sinking the parent; `--json` for CI pinning; exit 1 on divergence.
  - Secret-bearing engine flows replay green via NORMAL hashes (envelope pre-redaction makes the
    dataflow redaction-stable — see C-43's finding); the dual-hash path is covered at tape level.

## Notes
- Offline guarantee rests on the lazy provider (`main.rs:2289`) never constructing a client because
  no model op is reached (plans come from `plan_source`, leaf ops from the cassette).
