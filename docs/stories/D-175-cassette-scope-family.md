---
id: D-175
title: Cassette-scope family — Frozen, Resume, and world-pinned re-plan
pillar: Agent
status: backlog
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
- [ ] `CassetteScope` marked `#[non_exhaustive]`; two new arms `Frozen(FrozenTape)` and
      `Resume(ResumeTape)`, wired at the one dispatch chokepoint `ExecutorHost::dispatch`
      (`crates/flux-flow/src/runtime.rs` Record `:88` / Replay `:110` / confirm auto-allow `:158`).
- [ ] `ReplayTape::serve_nonlatching(op, input) -> Option<OpOutcome>`: same dual-hash matcher, but a
      *plain* miss returns `None` without latching; truncated + hash-mismatch-after-match still latch
      loudly (failing-first test for both behaviours).
- [ ] `Frozen` serves from tape with optional per-op substitutions; on miss either latch-and-halt
      (hermetic) or fall through to a live `RecordScope` (live-bridge), selected by an `OffTape` mode.
- [ ] `Resume` serves completed ops (exactly-once), falls through to live `executor.dispatch` +
      record for the tail; a served cell whose re-derived `input_hash` mismatches latches loudly.
- [ ] `run_turn_pinned(session, input, scope, sink)` on `FlowEngine` reuses the whole `run_turn` body
      and swaps **only** the `set_cassette` install (`engine.rs:420`) — no parallel turn path
      (no-fallbacks rule).
- [ ] `cargo test -p flux-codegate` (layering) and `cargo test -p flux-flow` green.

## Progress
- (not started — epic deferred; docs-only for now)

## Notes
- Files: `crates/flux-flow/src/cassette.rs` (`:119`), `runtime.rs`, `engine.rs`.
- SemVer: growing the public `CassetteScope` enum is breaking → MINOR (0.y). The `#[non_exhaustive]`
  marker makes all *future* arms additive.
- No user-visible SDK surface on its own; value lands when D-176/D-178 consume it.
