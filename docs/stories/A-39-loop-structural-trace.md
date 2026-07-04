---
id: A-39
title: "--trace-loop: flag-gated structural trace of the outer agent loop"
pillar: Agent
status: done
design:
epic:
note: loop.round/loop.node observations from the interpreter, sink-scoped to the outer loop; live-only, zero default-output change
---

# --trace-loop: flag-gated structural trace of the outer agent loop

## Goal
When the flux CLI runs a turn, the OUTER agent loop (`crates/flux-flow/assets/agent-loop.flux`:
orient → gather `repeat 3` → execute `repeat 25`) is invisible today — the only live signal is the
`loop.phase` spinner label, the repeat counter is discarded by the interpreter
(`for _ in 0..*max`), and the `[N/25]` in the transcript is an *inner tool-call* counter reusing
the outer cap as its denominator. Add a flag-gated structural trace: under `--trace-loop` /
`FLUX_TRACE_LOOP` the CLI prints one dim line per outer-loop round (`⟳ round 3/25`) and per
structural AST node executed (op calls with bind names, when/unless/match branches taken,
parallel branch entry, return, until-exit). Zero change to default output; pure computation nodes
(fmt/jq/expr/parse/lit/pure binds) are never traced.

## Approach (decided)
Observation-based and live-only, gated ONCE at emission via a default-false `FlowSink` method:

- `FlowSink::trace_structural() -> bool { false }` (`crates/flux-lang/src/sink.rs`) — the
  interpreter checks the sink it already holds at every `exec_body` site; no signature threading.
- The interpreter emits `loop.round` (repeat iterations, 1-based, with `max`) and `loop.node`
  (call/when/unless/match/return/parallel.branch/until-exit) Observations through the EXISTING
  `sink.observation` path — never `executor.observe`, so nothing lands in the evidence log or
  events.db (live-only by construction).
- Outer-loop scoping is by sink instance: `SinkBridge` (flux-flow) gains a `trace` flag; only the
  engine's outer `execute_flow_traced(...)` call sets it (from `trace_loop()` reading
  `FLUX_TRACE_LOOP`, exported by the CLI's `--trace-loop`). Inner `run_plan`, `flow run`, and
  resume paths keep their own SinkBridges at false — they are one boolean away from opting in
  later (deliberate follow-up seam).
- CLI: two new arms in `CliSink::observation` render the dim lines via a pure, unit-testable
  `trace_node_line` formatter. TUI needs no change (unknown observation kinds are ignored).

## Acceptance
- [x] Failing-first flux-lang test `structural_trace_emits_rounds_branches_calls_and_return_when_sink_opts_in`
      (runtime.rs mod tests): an agent-loop-shaped AST (repeat+until, match, when, return) run with
      an opted-in BufferSink yields the exact ordered `loop.round`/`loop.node` sequence with
      1-based counters and until-hit data.
- [x] Negative test: the same flow with a default sink emits ZERO trace observations — pins the
      zero-default-output invariant at the emission gate.
- [x] Parallel test: each branch's `parallel.branch` event arrives exactly once, in declaration
      order.
- [x] flux-flow gating test: `execute_flow` emits no `loop.*` kinds; `execute_flow_traced(..., true)`
      does (pins the SinkBridge seam the engine uses).
- [x] flux-cli: table-driven unit test for `trace_node_line` (call/bind, when/else, unless/skip,
      match/default, return, until-exit, parallel branch, unknown fallback).
- [x] `--trace-loop` on the agent-path subcommands + `FLUX_TRACE_LOOP` env; documented in
      `docs/agent-loop.md` beside `--show-loop`.
- [x] Workspace gate green: `cargo test`, `clippy -D warnings`, `cargo fmt --check` (both
      workspaces). *(2026-07-04 orchestrator pass: full root + plugins/ gate ran green — clippy
      clean, all tests 0 failed, fmt clean in both workspaces.)*

## Progress
- 2026-07-04 — filed with the design already validated against the live tree (plan-mode research:
  emission sites pinned in `exec_body`, SinkBridge construction sites enumerated, spinner
  interaction verified safe by the flow.halt precedent). Implementation starting.
- 2026-07-04 — implemented end-to-end, failing-first:
  - `crates/flux-lang/src/sink.rs`: `FlowSink::trace_structural() -> bool { false }` default method.
  - `crates/flux-lang/src/runtime.rs`: `trace_structural()`/`var_label()`/`truncate_json()` helpers;
    `BufferSink` gained a `trace: bool` field (+ `trace_structural` impl); emission sites wired at
    `Node::Repeat` (round counter + until-hit), the bind-of-call arm (before dispatch), the
    `Node::Call` statement arm, `Node::When`, `Node::Unless`, `Node::Match` (subject/value
    truncated to 60 chars/arm), `Node::Return`, and `Node::Parallel` (branch trace captured via
    `sink.trace_structural()` before the futures are built, replayed through each branch's own
    `BufferSink`). New tests: `structural_trace_emits_rounds_branches_calls_and_return_when_sink_opts_in`,
    `structural_trace_is_silent_by_default`,
    `structural_trace_emits_one_parallel_branch_event_per_branch_in_order` — all written first,
    confirmed to fail to compile (`BufferSink` had no `trace` field) before the field/emission
    sites landed.
  - `crates/flux-flow/src/runtime.rs`: `SinkBridge` gained a `trace: bool` field; new
    `execute_flow_traced(..., trace_structural: bool)` entry point; `execute_flow` now delegates to
    it with `false`. All 6 other `SinkBridge` construction sites pinned to `trace: false`. New test
    `execute_flow_traced_gates_structural_observations` (written first, failed to compile with "no
    function `execute_flow_traced`" before the fn existed) plus a `CollectSink.observations: Vec<String>`
    field to observe kinds.
  - `crates/flux-flow/src/engine.rs`: new `trace_loop()` reading `FLUX_TRACE_LOOP`, beside
    `show_loop()`; the turn's one `execute_flow` call site now calls
    `execute_flow_traced(..., trace_loop())`.
  - `crates/flux-cli/src/main.rs`: `AgentFlags::trace_loop` (`--trace-loop`, rides every
    `AgentFlags`-flattened subcommand via `#[command(flatten)]` — verified on `run`/`plan`/`tui`
    `--help`); `apply_agent_env` exports `FLUX_TRACE_LOOP`; `CliSink::observation` gained
    `loop.round`/`loop.node` arms (dim `eprintln!`, same idiom as the existing `loop.phase`/
    `flow.halt` arms); new pure `trace_node_line` formatter beside `halt_line` covering every
    documented shape (`call`/`when`/`unless`/`match`/`return`/`repeat`/`parallel.branch` +
    unknown-kind fallback) with table-driven test `trace_node_line_formats_every_structural_kind`
    plus a dispatch smoke test `loop_round_and_node_dispatch_without_panicking`.
  - `crates/flux-tui/src/lib.rs`: verified, NOT edited — its `observation()` `if/else if` chain has
    no trailing `else`, so `loop.round`/`loop.node` are already silently ignored.
  - `docs/agent-loop.md`: new "Trace the loop's structure — `--trace-loop`" subsection right after
    the `--show-loop` section, with a sample trace.
  - Gate: `cargo test -p flux-lang -p flux-flow -p flux-cli` → 81 + 209 + 3 + 1 + 222 + 1 + 3 + 2 +
    0 + 1 = all green, 0 failed; `cargo clippy -p flux-lang -p flux-flow -p flux-cli --all-targets
    -- -D warnings` → clean; `cargo fmt --check` on every touched file → clean.
  - Deviations from the design doc: none functionally — `var_label`/`truncate_json` are named
    slightly differently than the design's inline sketch but implement exactly the described
    behavior; the "unless"/"when" trace line format for a *missing* cond label (not specified by
    the design, since the design only showed the with-label case) was defined as `"· when → {branch}"`
    / `"· unless → {word}"` (segment omitted) rather than inventing a placeholder — a reasonable,
    self-consistent choice since no acceptance test exercises that path on the real
    `agent-loop.flux` program (its conds are always `$vars`).

## Notes
- Emission sites (flux-lang/src/runtime.rs `exec_body`): Repeat (enumerate the discarded counter),
  bind-of-call before `run_call`, Call statement, When after `eval_cond`, Unless, Match after arm
  selection (subject value truncated ~60 chars), Return, Parallel branch entry (into the branch's
  BufferSink — BufferSink gains a `trace` flag captured before the branch futures are built).
- Skipped node kinds (Each/Retry/Try/Route/Race/Seq) extend with the same one-line helper call —
  documented at the helper.
- Composite ops re-enter `execute_flow` with the same sink, so a workspace-override loop calling a
  composite traces the composite's internals too — benign (it IS the outer program's structure).
- No depth/indentation field in v1 — it would require parameter threading through ~30 recursion
  sites; every line carries its own context instead.
- Volume: worst case ≈ 180 dim lines per 25-round turn, only under the flag; zero allocation when
  off (the sink check precedes any json construction).
