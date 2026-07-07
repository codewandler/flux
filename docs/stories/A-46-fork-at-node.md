---
id: A-46
title: Fork-at-node + `flux fork` — branch a run at any decision and explore a different path
pillar: Agent
status: done
design: docs/designs/time-machine.md
epic: time-machine
note: "Time Machine Phase 2 SHIPPED 2026-07-07 — `flux fork <s> --at N` with inject/edit/replan; boundary = the Replay→Record scope swap, so forks are first-class replayable/diffable sessions; envelope-on-tail pinned by a deny-approver test"
---

# Fork-at-node + `flux fork`

## Goal
Branch a recorded run at any decision node into a new session: replay the prefix hermetically from
the cassette, then diverge — inject a different value, re-plan the tail live, or edit the plan text —
and continue. The counterfactual half of the Time Machine ("what if the agent had chosen
differently").

## Acceptance
- [ ] `FlowStore::fork_session(events, src, at) -> ForkHandle` (`crates/flux-flow/src/fork.rs`, L3):
      mint a new `s_<n>` with `correlation_id = src` (reusing the sub-agent linkage,
      `crates/flux-orchestrate/src/lib.rs:333`), reconstruct the prefix plan from `src`'s
      `plan_source`, compute `boundary_seq` (first `OpRecorded` after `StatementCompleted{node=at-1}`),
      and replay the prefix under `CassetteHost(Replay{boundary})` to rebuild symbols + values.
- [ ] The **cassette-vs-live boundary is `boundary_seq`**: every op below it is served from tape (no
      side effects); every op at/after runs through the **real** `Executor::dispatch` envelope
      (approval/authorization unchanged).
- [ ] Three divergence modes: **A inject** a value via `resume_flow_named` (`runtime.rs:918`,
      handles an `await` fork node natively); **B re-plan** the tail live via the real `plan` op;
      **C edit** the plan text and continue via the resumable-execution path
      (`run_top_level_resumable`, `runtime.rs:1108` — private; reached through the public
      `execute_flow_resumable*` wrappers, as `flux flow run --resume` does; denial-re-emission
      guard applies).
- [ ] `flux fork <SESSION|last> --at <NODE> [--inject <json> | --replan | --edit <file>] [-m …]
      [--yes]` (agent-path subcommand, flattens `AgentFlags` for the live tail). The forked session
      is a first-class `s_<n>` visible in `flux sessions`.
- [ ] Failing-first tests: (1) **fork reproduces prefix then diverges** — record a 3-statement flow;
      `fork --at 2 --inject` → statements 0–1 identical bindings, statement 2 differs; (2) **fork
      tail keeps the envelope** — fork with a mutating tail op + approval denied → tail refused,
      proving the boundary routes the tail through the real approver.
- [ ] Full gate green; layering intact.

## Progress
- 2026-07-07 DONE. `crates/flux-flow/src/fork.rs` (`replay_prefix` + `diverge_inject`/
  `diverge_edit`) + `flux fork` (agent-path subcommand; mode B = a live `engine.run_turn` on the
  fork session). Deliberate deltas vs the story text:
  - **No `boundary_seq`.** The cassette-vs-live boundary is a SCOPE SWAP: the prefix replays
    under `CassetteScope::Replay` (earlier executions in full, then the target plan truncated to
    `body[..at]`), and on success the store's scope flips to `CassetteScope::Record` for the fork
    session — simpler than cursor arithmetic AND it makes the forked tail record its own cassette,
    so forks are first-class replayable/diffable sessions (verified live: fork → delete artifact →
    `flux replay <fork>` reproduces without re-firing).
  - **`--inject` executes a synthetic bind-plan** (`bind name = lit(value)` + the remaining tail),
    not a store-level poke — the interpreter does the binding (D-67 lit-canonicalization parity),
    the ledger records it, and the fork stays self-contained for replay.
  - The fork session opens one turn (`<fork src @ N>`) and every plan it executes is recorded as
    an accepted `plan_source` attempt (modes A/C bypass the loop host's recorder) — found via the
    first live smoke, where the fork replayed as "0 plans"; fixed same day.
  - Fork session: `correlation_id = src` + `agent_id = fork:src@N` (the A-08 linkage `flux replay
    --sub-agents` and cost rollups already understand); the parent's conversation Messages are
    copied so mode B's planner has the recorded context.
  - Tests: `fork_replays_prefix_then_diverges_by_injection` (prefix from tape — no write re-fire;
    injected symbol bound; the live tail's fresh cell on the fork stream proves real dispatch) and
    `fork_tail_keeps_the_envelope` (deny-all approver + empty permissions → tail refused with
    `Denied`/`ConfirmDenied`, zero executions). Mode C smoked live end-to-end (edited plan ran
    through the envelope, wrote a real file, then replayed hermetically). Full gate green.
  - Residual: mode B was not live-smoked (it is a plain `engine.run_turn`, the most-exercised path
    in the repo); `--at` addresses the FINAL executed plan (earlier-plan fork points would need a
    `--plan` selector — file on demand).

## Notes
- The one-shot suspension latch is NOT copied on fork (a fork starts un-suspended; an `await` fork
  node is handled by inject-mode). Mode B is nondeterministic by design (temperature None) — that is
  the intended semantics of "what if the model chose differently."
