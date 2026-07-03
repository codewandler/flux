---
id: L-29
title: "Gather effect gate must block all non-Read effects, not just Write/Destructive"
pillar: Language
status: done
epic: library-hardening
design: docs/designs/library-hardening.md
note: "mutating_ops_in flags an op only if Destructive or Effect::Write, but the approval path and optimizer treat any non-Read effect as mutating; the phase never restricts the catalog, so a gather:true 'read-only orient' round can emit and execute a Network/Process/Browser/LocalSystem op"
---

# Gather effect gate must block all non-Read effects, not just Write/Destructive

## Goal
Actually enforce the read-only contract of the A-13 gather phase. The gate uses `mutating_ops_in`, which
flags an op iff `sig.risk == Risk::Destructive || sig.effects.contains(&Effect::Write)`
(`crates/flux-flow/src/registry.rs:149`). But the `Effect` enum also has `Network`, `Process`, `Browser`,
`LocalSystem` (`flux-spec/src/lib.rs:32`), and both the plan-approval path (`accumulate_risk`,
`crates/flux-flow/src/runtime.rs:464`) and the optimizer's `is_readonly_op` (`optimize.rs:153`) treat *any*
non-`Read` effect as mutating. The phase does not restrict the advertised catalog, so a `gather:true` round
can emit an advertised `[Network]` op (http, `run_plan` — `crates/flux-tools/src/reflect.rs:152`) or a
`[Process, LocalSystem]` op (cargo/shell) and it passes `gather_violation` (`compile.rs:1070`) and executes —
the "approval-free, read-only orientation" contract broken.

## Acceptance
- [ ] Failing-first test: a `gather:true` plan calling an advertised non-`Read`-effect op (e.g. a `Network`
      op) is rejected by the gather gate (today it compiles and would execute).
- [ ] Fix: `mutating_ops_in` rejects any op whose effects aren't a subset of `{Read}` (mirror
      `is_readonly_op`), instead of testing only `Write`/`Destructive`.
- [ ] Existing read-only gather plans still pass; the approval-path and gather-gate notions of "mutating" now agree.

## Progress
- 2026-07-03 DONE — `mutating_ops_in` now flags any op carrying a `Write`/`Network`/`Browser`/`LocalSystem` effect or `Destructive` risk, keeping `Read`/`Filesystem`/bare-`Process` (e.g. `git_status`) gather-safe — closes http/run_plan/cargo/bash escaping a gather round while sparing real Read+Filesystem reads. Tests: `network_effect_op_is_flagged_mutating`, `process_and_local_system_effect_op_is_flagged_mutating`, `read_only_op_is_not_flagged_mutating` (existing compile.rs gather tests pass unmodified). Orchestrator note: corrected the lane's initial all-`Read` mirror, which would have wrongly flagged real Read+Filesystem read ops. Full gate green.

## Notes
- Evidence: `crates/flux-flow/src/registry.rs:149`; `flux-spec/src/lib.rs:32`;
  `crates/flux-flow/src/runtime.rs:464`; `crates/flux-flow/src/optimize.rs:153`;
  `crates/flux-flow/src/compile.rs:1070`; `crates/flux-tools/src/reflect.rs:152`.
- Residual of [A-13](A-13-phase-aware-planner-protocol.md) / [L-17](L-17-runtime-semantics-hardening.md).
  Design: [library-hardening](../designs/library-hardening.md).
