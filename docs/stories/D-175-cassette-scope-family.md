---
id: D-175
title: Cassette-scope family — Frozen, Resume, and world-pinned re-plan
pillar: Agent
status: done
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
note: "Phase 2 — the shared engine spine; MINOR (public CassetteScope grows)"
---

# Cassette-scope family — Frozen, Resume, and world-pinned re-plan

## Goal
Generalize the engine's `CassetteScope` (today `Record`/`Replay`) into a small family that lets a
turn re-execute against a byte-frozen recorded world (with substitutions) or resume completed ops
exactly-once — the single shared primitive under Tune (D-176) and Resurrect (D-178).

## Acceptance
- [x] `CassetteScope` marked `#[non_exhaustive]`; two new arms `Frozen(FrozenTape)` and
      `Resume(ResumeTape)`, wired at the one dispatch chokepoint `ExecutorHost::dispatch`
      (`crates/flux-flow/src/runtime.rs` Record `:88` / Replay `:110` / confirm auto-allow `:158`).
- [x] `ReplayTape::serve_nonlatching(op, input) -> Option<OpOutcome>`: same dual-hash matcher, but a
      *plain* miss returns `None` without latching; truncated + hash-mismatch-after-match still latch
      loudly (failing-first test for both behaviours).
- [x] `Frozen` serves from tape with optional per-op substitutions; on miss either latch-and-halt
      (hermetic) or fall through to a live `RecordScope` (live-bridge), selected by an `OffTape` mode.
- [x] `Resume` serves completed ops (exactly-once), falls through to live `executor.dispatch` +
      record for the tail; a served cell whose re-derived `input_hash` mismatches latches loudly.
- [x] `run_turn_pinned(session, input, scope, sink)` on `FlowEngine` reuses the whole `run_turn` body
      and swaps **only** the `set_cassette` install (`engine.rs:420`) — no parallel turn path
      (no-fallbacks rule).
- [x] `cargo test -p flux-codegate` (layering) and `cargo test -p flux-flow` green.

## Progress
- **2026-07-27 — implemented.** `crates/flux-flow/src/cassette.rs`: `CassetteScope` is
  `#[non_exhaustive]` with new `Frozen(FrozenTape)`/`Resume(ResumeTape)` arms; `OffTape{Halt,Live}`;
  the `ReplayTape::serve` matcher core is extracted into a private `serve_result -> ServeResult`
  (`Served`/`Truncated`/`Miss`) + `latch`/`latch_first` helpers, `serve` is byte-identical, new
  `serve_nonlatching` latches only on `Truncated`. `FrozenTape` (`hermetic`/`live_bridge`
  constructors, `substitute_op`/`substitute_cell`, `with_reauthorize`, `went_live`/`substituted`
  counters, `policy_denials`/`note_policy_denial`, `is_hermetic()`) and `ResumeTape` (`new`, `served`/
  `ran_live` counters, `remaining`/`diverged`) both share a `ScopeServe{Served,Refused,Miss}` result
  consumed by the ONE dispatch chokepoint. `crates/flux-flow/src/runtime.rs`: `record_cell` generalized
  to `tail_record` (Record / Frozen-Live-bridge / Resume-tail arms); `ExecutorHost::dispatch` matches
  every scope arm before the one live path (Served/Refused early-return via a new `cassette_refused`
  in-band-error shaper, every Miss falls into the same live path); `request_approval` per-arm table
  (Replay + Frozen(Halt) auto-allow, Record/Frozen(Live)/Resume gate through the real approver).
  `crates/flux-flow/src/engine.rs`: threaded `scope_override: Option<Arc<CassetteScope>>` through
  `run_turn_locked` → `run_turn_lifecycle` → `begin_turn_lifecycle` (existing callers, incl.
  `start_flow_turn_locked`, pass `None`); the cassette install at `begin_turn_lifecycle` is the ONLY
  swapped code (`Some(scope)` wins over `FLUX_CASSETTE=0`); new `pub async fn run_turn_pinned` mirrors
  `run_turn`'s body with a fresh `CancellationToken` — no parallel turn path.
  Tests: 22 new in `cassette.rs` (`serve_nonlatching` miss/truncated/hit, `Frozen` substitution-by-cell-
  wins-over-op, unsubstituted-serves-recorded, Halt-miss-latches, Live-miss-is-passthrough, truncated-
  never-falls-through-live, `is_hermetic()` live/denial tracking, `reauthorize` default+builder,
  `Resume` hit-exactly-once, truncated-refuses, plain-miss-never-latches), 6 new in `runtime.rs`
  (Frozen-Halt never touches the executor, Frozen-Live dispatches live exactly once + records the
  bridge tail, Resume serves without refiring, Resume miss runs live + records the tail, Replay+
  Frozen-Halt auto-allow a `confirm` without consulting a deny-approver, Frozen-Live+Resume gate a
  `confirm` through a real deny-approver — proves the `:158` auto-allow doesn't leak), 2 new in
  `engine.rs` (`run_turn_pinned` produces a normal `TurnStarted`/`TurnEnded` shape and resets the
  cassette on finish; the pinned scope wins over `FLUX_CASSETTE=0`, proven via a scripted adaptive-loop
  turn with a counting `echo` tool). `cargo test -p codewandler-flux-flow --lib`: 167 passed;
  `cargo clippy -p codewandler-flux-flow --all-targets -- -D warnings`: clean; `cargo fmt -p
  codewandler-flux-flow`: clean; `cargo test -p flux-codegate`: 11 passed (layering unaffected — no new
  crate, no new cross-crate edge). Deviation from the plan: the `Frozen`/`ResumeTape` boolean hermetic
  accessor is named `is_hermetic()`, not `hermetic()` (the plan's `FrozenTape::hermetic(tape)`
  constructor already owns that name — renamed for `E0592`, no functional change).
  D-176/D-178 construction notes: `FrozenTape::hermetic(ReplayTape::from_trace(&trace))` /
  `FrozenTape::live_bridge(tape, RecordScope::new(events, session))` +
  `.substitute_op(op, outcome)`/`.substitute_cell(index, outcome)` builders;
  `ResumeTape::new(cells, RecordScope::new(events, session))` where `cells` is the caller-built
  crash-tail slice; both share `ScopeServe` and are consumed uniformly by `ExecutorHost::dispatch` —
  no engine.rs changes needed beyond `run_turn_pinned(session, input, Arc::new(scope), sink)`. Note the
  ONE remaining architectural gap (pre-existing, not this story's scope): `run_turn_pinned` only swaps
  `set_cassette` for `TurnProgram::Adaptive`/`Resume` (mirroring `run_turn`); the outer adaptive loop's
  OWN stage dispatches (`declare_intent`/`explore`/…) run through `execute_flow_traced`, which has
  never self-wired the store's cassette — only nested authored/action-batch execution
  (`execute_flow_with_composites`, e.g. `execute_batch`'s tool calls) does. A pinned `Frozen`/`Resume`
  scope therefore governs leaf-op dispatch (the case Tune/Resurrect care about) but not the outer
  loop's own native-op calls; confirmed by test and worth flagging if D-176's `check()` needs to pin
  something at that outer layer too.

## Notes
- Files: `crates/flux-flow/src/cassette.rs` (`:119`), `runtime.rs`, `engine.rs`.
- SemVer: growing the public `CassetteScope` enum is breaking → MINOR (0.y). The `#[non_exhaustive]`
  marker makes all *future* arms additive.
- No user-visible SDK surface on its own; value lands when D-176/D-178 consume it.
