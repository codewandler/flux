---
id: D-182
title: Record served cells on the what-if re-plan path so its diffs are never vacuous
pillar: Agent
status: done
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
priority: 2
note: "review finding (2026-07-28): re-plan path drives run_turn_pinned with a bare NullSink — a fully tape-served re-plan diffs as total divergence"
---

# Record served cells on the what-if re-plan path so its diffs are never vacuous

## Goal
`WhatIf::run`'s **re-plan** path (`.model()`/`.system_prompt()` variants) drives
`run_turn_pinned(&dst, …, &mut sink)` with a plain `NullSink` (`crates/flux-sdk/src/whatif.rs`).
Under a `Frozen` scope a served dispatch returns from `ExecutorHost::dispatch` before the tail
record, so no `OpRecorded` cell is written to the destination session. `Scenario::check` faces the
identical gap and fixes it explicitly with `flux_flow::whatif::RerunRecordingSink`; the re-plan path
must do the same.

Confirmed failure: a re-plan whose new plan is identical and fully tape-served yields
`hermetic() == true` while `diff()` (via the `cell_rows` fallback) sees zero cells on the dst side
and reports every statement as `Plan { b_stmt: None }` — a fake total divergence,
`first_divergence()` at node 0, and `what_if_over` over-counting `changed`. Both existing re-plan
tests only exercise genuinely-diverging plans, so the vacuous case is unpinned. The same shape
affects `.off_tape(Live)` substitution runs (`FrozenTape::record_tail` records misses, never hits).

## Acceptance
- [x] The re-plan path wraps its sink in `RerunRecordingSink` (or equivalent) so served dispatches
      are recorded into the destination session, exactly as `Scenario::check` does.
- [x] Failing-first test: a `.system_prompt()` variant engineered to produce the identical plan,
      fully tape-served → `diff().identical == true` (not total divergence), `hermetic() == true`.
- [x] `.off_tape(Live)` runs record served hits too, not only live-miss tails; test pins a mixed
      served+live run's diff completeness.
- [x] While in the file: `RerunRecordingSink` hardcodes `denied: false` (the `ToolResult` bridge
      drops the structural flag), so a D-177 reauthorize denial is re-recorded as a plain retryable
      error — carry the denial through or document the classification loss at the recording site.
- [x] Handed over from D-184 (same file, `crates/flux-sdk/src/whatif.rs`): `substitute_at` with a
      node id that maps to no recorded dispatch returns an error naming the node, never a silent
      identical run; test pins it.

## Progress
- 2026-07-28: **Item 1 (re-plan self-recording).** `WhatIf::run`'s re-plan branch
  (`crates/flux-sdk/src/whatif.rs`) now wraps the caller's sink in
  `flux_flow::whatif::RerunRecordingSink` before calling `run_turn_pinned`, exactly as
  `Scenario::check` does — `enabled = true` unconditionally, since nothing else ever records a
  served hit on `dst`. Failing-first test `replan_with_an_identical_plan_is_fully_served_and_diffs_identical`
  (`crates/flux-sdk/tests/whatif.rs`) drives a `.system_prompt()` re-plan onto the SAME op via
  `AltPlanMock` (a system prompt that avoids the mock's alt-marker branch), fully tape-served, and
  asserts `diff().identical == true`, `hermetic() == true`, and that the served cell is actually
  present in the counterfactual's own trace (ruling out "two empty traces compare equal" as a false
  pass). Confirmed this failed before the fix (`diff().identical == false`, a synthetic total
  divergence) and passes after.
- 2026-07-28: **Item 2 (`.off_tape(Live)` served-hit recording) — required a deeper fix than the
  Acceptance text implied.** The obvious fix (compare a `FrozenTape::went_live()` counter's value at
  `tool_call` vs `tool_result` time to tell a served dispatch from a live one, skip self-recording
  the live ones) turned out to be UNSOUND: a real re-plan drives the full adaptive agent loop, and
  native tool dispatches inside its `explore` composite reach `RerunRecordingSink` through an
  internal channel relay (`loop_host::ChannelSink`) that preserves dispatch ORDER but not its
  wall-clock timing relative to the live dispatch itself — a counter snapshotted at `tool_call` time
  can already be stale by the time `tool_result` fires. Caught this empirically: the real-time-delta
  version silently DOUBLE-recorded a live dispatch in a mixed served+live re-plan test. Replaced it
  with deferred, reconciled recording: `RerunRecordingSink::defer_for_live_bridge()` (opt-in, only
  needed under `OffTape::Live`) buffers every dispatch instead of writing eagerly;
  `RerunRecordingSink::finish()` — called once, after the driven turn/plan has fully settled — reads
  back what `FrozenTape::record_tail`'s live-bridge already wrote to the same session and merges its
  own buffered dispatches against it in order, self-recording only the ones that don't match (the
  served ones). This guarantees COMPLETENESS (every dispatch recorded exactly once — no drop, no
  duplicate) but trades away exact POSITIONAL order for a deferred (served) cell relative to a
  synchronous (live) one, since an append-only event log can't retroactively insert a cell before
  one already written — documented explicitly on `defer_for_live_bridge`/`finish`
  (`crates/flux-flow/src/whatif.rs`) as a known, narrower limitation than full `diff()` alignment,
  not silently shipped. Wired into `rerun_pinned` (self-record now enabled for `Frozen` regardless of
  `off_tape`, deferred only under `Live`) and into the SDK re-plan path
  (`crates/flux-sdk/src/whatif.rs`) the same way. Also added `RecordScope::recorded_cells()`
  (`crates/flux-flow/src/cassette.rs`, `pub(crate)`) as the read-back `finish()` needs. Two failing-
  first tests: `off_tape_live_replan_records_both_served_and_live_cells`
  (`crates/flux-sdk/tests/whatif.rs`, exercises the real agent-loop composite path — asserts
  completeness, i.e. both ops present exactly once, not exact order) and
  `rerun_pinned_off_tape_live_records_both_the_served_and_the_live_cell_completely`
  (`crates/flux-flow/src/whatif.rs`'s own test module, exercises `rerun_pinned` directly on a raw
  two-statement AST — same completeness assertion; this one ALSO demonstrated that even a raw,
  non-composite flow lands the deferred cell after the live one, confirming the ordering trade-off is
  inherent to deferral, not specific to the agent-loop relay).
- 2026-07-28: **Item 3 (`denied: false` hardcoded).** Investigated widening what
  `RerunRecordingSink::tool_result` sees: the structural `denied` flag lives on
  `flux_lang::host::OpOutcome`, but the bridge every `AgentSink` implementation actually receives is
  `flux_runtime::ToolResult` (`content`/`view`/`is_error` only, no `denied`) — it is dropped one layer
  up in `SinkBridge::tool_result` (`crates/flux-flow/src/runtime.rs`), which is OUTSIDE this story's
  file ownership (a concurrent agent's territory per the task brief), and `ToolResult` itself is a
  `flux_runtime` type used far beyond this one sink. Widening either is out of scope for this pass.
  Documented the loss precisely at the recording site (`RerunRecordingSink::tool_result`,
  `crates/flux-flow/src/whatif.rs`): a D-177 reauthorize denial served through this recording sink is
  re-recorded as a plain `is_error` outcome, indistinguishable here from an ordinary op failure. Noted
  as NOT a correctness bug in `hermetic()`/`diff()` — both key off the LIVE `FrozenTape` denial path
  (`policy_denials()`), never off this recorded cell — so the loss is confined to the recorded cell's
  own `denied` flag being wrong, not to the honesty gates the Lab actually relies on.
- 2026-07-28: **D-184 hand-off (`substitute_at` silent no-op).** Fixed in the same pass, same file
  (`crates/flux-sdk/src/whatif.rs`): `build_frozen` now returns `Result<FrozenTape>` and
  `node_to_cell_index(trace, node) == None` becomes an error naming the node, instead of silently
  skipping the substitution. Both `build_frozen` call sites in `WhatIf::run` propagate with `?`.
  Failing-first test `substitute_at_a_dead_node_errors_instead_of_silently_no_opping`
  (`crates/flux-sdk/tests/whatif.rs`) targets a node id (`9999`) nothing in the recorded plan ever
  bound to, and asserts the error names it. Ticked the corresponding checkbox in
  `docs/stories/D-184-lab-honesty-gaps.md` with a note pointing back here, per the hand-off — no
  other change made to that story.
- Gate (package-scoped — a concurrent agent is mid-flight elsewhere in the workspace, per the task
  brief; verified only what this story touches): `cargo test -p codewandler-flux-flow` (186/186),
  `cargo test -p codewandler-flux-sdk --features test-kit` (all suites green, `whatif.rs` 11/11
  including the 4 new tests), `cargo test -p codewandler-flux-sdk` (default features, 58/58 lib +
  integration suites green), `cargo clippy -p codewandler-flux-flow --all-targets -- -D warnings`,
  `cargo clippy -p codewandler-flux-sdk --features test-kit --all-targets -- -D warnings`, `cargo
  clippy -p codewandler-flux-sdk --all-targets -- -D warnings` (all clean), `cargo fmt --all --
  --check` (clean, both root and `plugins/` workspaces). Did not run the full `cargo test --workspace`
  gate given the concurrent agent's in-flight edits elsewhere; nothing outside this story's owned
  files was touched or verified.

## Notes
- `crates/flux-sdk/src/test.rs` (`Scenario::check`) contains the reference wiring and a comment
  explaining the "every statement vanished" failure mode — the fix is reuse, not invention.
- The substitution-only path (`rerun_pinned`) already self-records under Halt and is sound; only the
  re-plan and Live-bridge paths are affected.
- Residual, documented limitation from item 2: under `.off_tape(Live)`, a self-recorded (served)
  cell's POSITION in the destination session's trace is not guaranteed to match true execution order
  relative to a live-bridge cell (it always lands after) — `diff()`/`cell_rows` alignment can
  therefore misattribute node positions in a mixed served+live re-plan. Completeness (nothing
  dropped, nothing duplicated) is guaranteed; exact position is not. A follow-on story could recover
  it by teaching `RerunRecordingSink::finish` to consult the plan's own step trace for ordering —
  out of this pass's scope, flagged rather than silently accepted.
