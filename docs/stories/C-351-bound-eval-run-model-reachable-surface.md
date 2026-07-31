---
id: C-351
title: Bound eval_run's model-reachable surface and bind it to session confinement
pillar: Core
status: backlog
epic: egress-pinning-and-confinement-residuals
design: docs/designs/egress-pinning-and-confinement-residuals.md
note: "the C-258 fix holds — flux_bin is rejected and credentials are allow-listed — but the schema has no additionalProperties:false, so trials/concurrency/timeout_secs/members are model-reachable and trials has a floor with NO ceiling"
---

# Bound `eval_run`'s model-reachable surface and bind it to session confinement

## Goal

Make `eval_run` execute only what it declares, and confine its children with the System the session
configured rather than whatever the host process's environment happens to say.

## Acceptance

- [ ] `EvalRunInput` (`crates/flux-eval/src/ops.rs:41-57`) declares every parameter `run_eval`
      actually reads, and the emitted schema is closed — today `trials`, `concurrency`, `watch`,
      `members`, `timeout_secs` and `agent_timeout_secs` reach the implementation undeclared.
- [ ] `trials` has a ceiling. It currently has a floor only (`ops.rs:145-149`) and expands into a
      `tasks × trials` vector (`:167-170`) — model-driven memory growth plus unbounded credentialed
      child-process and provider spend.
- [ ] `execute` consumes its `ToolContext` (`ops.rs:80` takes `_ctx`) so confinement comes from the
      session's System, not `SandboxSettings::from_env()` of the host process
      (`runner.rs:344`, `terminal_bench.rs:438`).
- [ ] The terminal-bench adapter's System is rooted at an eval-scoped workspace, not
      `std::env::current_dir()` (`terminal_bench.rs:435-437`).
- [ ] Failing-first regression: a model-shaped call carrying an undeclared parameter is rejected;
      an oversized `trials` is rejected; and a child spawned from a session whose System is confined
      is confined.

## Progress

- 2026-08-01 — filed from validation of PROC-01. The original allegation is closed; these are the
  adjacent widenings the fix did not cover.

## Notes

- The two exemption guards here are lexical and locally scoped: `model_reachable_eval_runner_has_no_sandbox_exemption`
  (`runner.rs:542-549`) greps only `runner.rs`, and `the_exempt_doc_names_exactly_the_seams_that_exist`
  (`sandbox.rs:1576`) never enumerates callers. A new `run_with_env_exempt` call in `ops.rs` or
  `terminal_bench.rs` passes both — worth folding into C-366.
