---
id: A-28
title: "BUG: window-sliding reads defeat the A-20 resource stall guard — freshness must be coverage-based, not key-based"
pillar: Agent
status: done
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: "s_355 (post-A-20 binary): 25 rounds re-reading ONE file at ever-shifting offsets (2180→2990) under new symbols — every window is a new op+args ReadTracker key, so round.fresh > 0 resets resource_stall EVERY round and the guard never arms; freshness for windowed reads must mean 'covered new lines', not 'new argument tuple'"
---

# BUG: window-sliding reads defeat the A-20 resource stall guard

## Goal
Close the loophole A-20 left open. The A-20 `ReadTracker` keys a read by `op + resolved-args JSON`
(`crates/flux-flow/src/runtime.rs:98`, `resource_key`), and `guard_resources`
(`crates/flux-flow/src/loop_host.rs:696`) resets `resource_stall` to 0 whenever `round.fresh > 0`.
For windowed ops (`read` with `offset`/`limit`), every slid window is a brand-new key, so a model
that never repeats an exact `(path, offset, limit)` tuple registers "fresh evidence" every round and
the escalate@2 / stop@3 ladder never arms. Freshness for the read-family must mean **the dispatch
contributed lines not already covered this turn** (an interval set per path), not "a new argument
tuple". A first pass paging through a large file stays fresh (new lines every window); s_355-style
re-grinding over an already-covered region does not.

## Forensics (s_355, 2026-07-03 18:46–19:00, binary built 18:38 — A-20 fix present)
- Turn 1 "bug: /plan mode cannot be left"; turn 2 "why do you read the same shit again and again
  without concluding". 25 `plan_attempted`, 277 `run` events, both turns user-cancelled.
- Every round: `read` on `crates/flux-cli/src/main.rs` at a new offset (2180, 2650, 2660, 2670,
  2700, 2749, 2750, 2810, 2830, 2950, 2990, …), fresh symbol names each time, occasional re-greps.
- **Zero** `[loop-guard]` / "already read as $X" / "No NEW evidence" strings across all 277 run
  events — neither the A-05/A-20 caches nor the transcript/resource stall guards ever fired.
- Method: `sqlite3 ~/.flux/events.db` over `stream='s_355'` (plan_attempted plan_text, run
  payloads, ts range).

## Acceptance
- [x] Failing-first test (mirror `fresh_read_resets_resource_stall`,
      `crates/flux-flow/src/loop_host.rs:5003`): a loop whose every round reads the SAME path at a
      new `offset` (no new lines after the file is covered) escalates at `RESOURCE_STALL_ESCALATE`
      and force-stops at `RESOURCE_STALL_STOP`. Today the fresh-key reset spins it to the repeat
      budget.
- [x] Fix: per-path covered-interval tracking in `ReadTracker` for windowed reads — a read is
      *fresh* only if it adds uncovered lines (or the file changed via the existing
      write-invalidation). Non-windowed read ops keep the op+args key semantics.
- [x] Legitimate paging stays unpunished: a test where each window advances through NEW regions of
      a large file never trips the guard (first full pass ≈ N windows, all fresh).
- [x] The escalate/stop messages name the covered file(s) so the model (and the user) see *why*
      ("you have already read lines X–Y of `main.rs`").
- [x] The A-20 rename-variation fixture still converges (≤ its current round bound).

## Progress
- 2026-07-03 filed from live s_355 forensics minutes after the runaway, during D-46 work. Root
  cause confirmed in code: `resource_key` includes `offset`/`limit`, and `guard_resources` treats
  any unseen key as evidence-progress (`round.fresh > 0` → counter reset). A-20 fixed the *rename*
  variation (s_346); this is the adjacent *argument-sliding* variation on one file.
- 2026-07-03 **DONE.** `ReadTracker` gains per-path covered-line intervals: `read_window` (input
  shape `path` + optional `offset`/`limit`, nothing else) + `add_coverage` (normalized interval
  set; fresh iff uncovered lines were added); `record` decides freshness by coverage for windowed
  reads and by exact key otherwise; the exact-key reuse cache is untouched; write-invalidation
  clears coverage. `guard_resources` escalate/stop feedback now names the covered files + line
  spans (`coverage_summary`). Tests (failing-first `sliding_window_reads_escalate_and_force_stop`
  reproduced the s_355 non-escalation exactly, then green): `paging_through_new_windows_never_stalls`
  (no-false-positive pin), `coverage_freshness_is_by_uncovered_lines`,
  `read_window_matches_only_the_read_shape`; all four A-20 pins pass unchanged. flux-flow gate
  green (193 lib tests); workspace suite green (87 binaries).

## Notes
- Same class as [A-20](A-20-stall-guard-resource-aware.md) (residual); coverage tracking composes
  with — does not replace — the exact-repeat read cache ("already read as $X — reusing").
- The underlying user bug from s_355's turn 1 ("/plan mode cannot be left" in the REPL) is a
  separate flux-cli issue — verify and file independently if it reproduces.
