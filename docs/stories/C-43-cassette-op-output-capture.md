---
id: C-43
title: Op-output cassette capture — RunEvent::OpRecorded + CassetteHost(Record)
pillar: Core
status: done
design: docs/designs/time-machine.md
epic: time-machine
note: "Time Machine Phase 0 SHIPPED 2026-07-07 — durable redacted op-output cells (RunEvent::OpRecorded riding EventKind::Run) captured at the one dispatch chokepoint; default ON (measured ~442 B/cell), FLUX_CASSETTE=0 opt-out, 1 MiB cap"
---

# Op-output cassette capture — RunEvent::OpRecorded + CassetteHost(Record)

## Goal
Make op OUTPUTS durable so a recorded run can later be replayed with zero model calls and zero live
IO. Today values live in an ephemeral in-memory store and only references persist
(`crates/flux-flow/src/state.rs:161`), so `RunEvent::StepSucceeded{output:ValueId}` points into a
store that dies with the process. This story adds the redacted op-output "cassette" — the single new
capture the whole Time Machine epic rests on.

## Acceptance
- [ ] New `RunEvent::OpRecorded { seq, step, op, input_hash, input_hash_redacted, content, view,
      is_error, denied, redacted, truncated }` in `crates/flux-lang/src/ast.rs` (L0), every new field
      `#[serde(default)]` so all existing on-disk rows still decode (assert with a back-compat test
      mirroring `kind.rs:270`). Rides `EventKind::Run` — no new `EventKind` arm, no new table.
      `input_hash_redacted = sha256(redact(input))` + `redacted: bool` exist because the live run
      hashes/binds UNredacted data while replay serves redacted content — the replay guard (A-45)
      must be able to match on the redacted hash for cells downstream of a redacted output.
- [ ] `CassetteHost` OpHost decorator (`crates/flux-flow/src/cassette.rs`, L3) with
      `Off | Record | Replay` (Replay stubbed for C-43 / implemented in A-45); self-installs from the
      executor context at every `ExecutorHost` construction (inner `loop_host.rs:1501`; outer
      `engine.rs:244` fallback for >32K-dropped plans). Record: dispatch → redact `content`/`view`
      via the executor's existing `Redactor` → append `OpRecorded` → return the UNredacted outcome to
      the live run.
- [ ] `input_hash` computed identically to `execute_call` (`runtime.rs:348`) so `StepId`s line up;
      `seq` is a per-session monotonic dispatch ordinal.
- [ ] Default decided from evidence, not assumed: measure `events.db` growth on a representative
      coding session with capture on, then pick on-at-1-MiB / smaller-cap / opt-in and record the
      measurement in Progress. Opt-out `--no-cassette` / `FLUX_CASSETTE=0`; per-op cap
      (`FLUX_CASSETTE_MAX_BYTES`) keeps the head with `truncated=true`.
- [ ] Failing-first **redaction test**: a secret seeded in the redactor and echoed by a `bash` op →
      every `OpRecorded.content` in `events.db` is redacted (mirror `engine.rs:2595`).
- [ ] Full gate green (both workspaces); layering intact (`flux-codegate`).

## Progress
- 2026-07-07 DONE. Implementation deltas vs the story text, all deliberate:
  - The "CassetteHost decorator" landed as an optional `cassette` FIELD on the existing
    `ExecutorHost` consulted inside `dispatch()` (the exact A-20 `reads` precedent) — less code,
    same seam, no trait-forwarding boilerplate. Scope rides on `FlowStore`
    (`set_cassette`/`cassette`), self-wired by the two plan-execution entry points
    (`execute_flow_with_composites`, `execute_flow_resumable_with_composites`); the outer
    agent-loop path (`execute_flow_traced`) is deliberately unwired, and `SKIP_OPS`
    (`plan`/`run_plan`) is belt-and-braces on top.
  - BOTH dispatch return paths record — including the A-20 cache-served repeat, or replay would
    see a hole in the tape.
  - Armed per agent turn in `run_turn_cancellable` AND on the `flux flow run` path (which now
    also persists its executed plan as an accepted `plan_source` attempt — that path has no loop
    host; its no-composites branch was routed through the wired entry point).
  - `sha256_hex` and `flow_key` made `pub` in flux-lang (shared derivations, the stmt_hash16
    precedent).
  - EMPIRICAL FINDING: the envelope's existing result-scrub (C-13) plus the `plan` op's own
    output scrub make engine-path dataflow redaction-stable end to end (the test proved the tool
    received `[redacted]`, not the secret) — so the dual-hash matcher is belt-and-braces there,
    load-bearing mainly for authored-flow edges; the redaction test asserts the STRONGER reality
    (no cell field, not even the input hash, derives from the raw secret).
  - Cost evidence for default-ON: ~442 bytes/cell average across the recorded smoke ops; cells =
    0.01% of a heavily-used events.db payload. Retention sweep remains a follow-up candidate
    (C-18 TTL precedent) before cassette-heavy server deployments.
  - Tests: 5 tape unit tests (order/out-of-order/dual-hash/divergence/truncation) +
    `cassette_records_redacted_cells_for_dispatched_ops` (E2E through a real turn) +
    `cassette_replay_serves_recorded_cells_without_refiring_side_effects` +
    `op_recorded_minimal_fields_decode_with_defaults` (back-compat). Full workspace gate green.

## Notes
- The `plan` op needs no capture on the primary path (its output is already durable as
  `PlanAttempted.plan_source`); the outer install site records only the rare >32K-dropped plan.
- Do NOT redact into a form that breaks parseability if the content is Flux-Lang — redaction stays
  inside string literals, same posture as `plan_source` (C-22).
