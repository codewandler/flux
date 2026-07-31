---
id: C-378
title: flow_check — an exact-flow preflight that separates inspectable from executable
pillar: Agent
status: backlog
epic: harness-route-integrity
design: docs/designs/harness-route-integrity.md
note: "no preflight of any kind exists; lowering happens inside run_authored_flow AFTER the decision to execute, against the FULL registry, so a flow needing an unsurfaced family lowers cleanly and then fails at dispatch"
---

# `flow_check` — an exact-flow preflight

## Goal

Let the agent answer "is this specific flow runnable here, right now, under this approval posture"
before it mutates anything.

## Acceptance

- [ ] A read-only `flow_check(name | path)` returns `{resolved_path, parse_ok, lower_diagnostics,
      required_ops, ops_registered, ops_visible, missing_families, per-op gather-vs-capture
      disposition, approval_posture, blockers[]}`, reusing `flux_lang::analyze::lower` against
      `executor.registry()` plus the executor's visibility check.
- [ ] `inspectable` and `executable` are separate fields, so the distinction cannot be flattened
      into prose.
- [ ] Failing-first fixtures for a flow whose ops are (i) registered and visible, (ii) unregistered,
      (iii) registered but outside `with_tools`, and (iv) unparseable — four distinct typed blockers.
- [ ] The op's own result wording states that a passing preflight is a static prediction, not a
      guarantee of execution.

## Progress

- 2026-08-01 — filed from validation of HAR-05. `rg -i preflight` across the tree hits only plugin
  protocol code, scripts, docs and the review documents themselves.

## Notes

- `flow_list` already surfaces a per-entry `[flow|op|error]` kind and refuses op-only targets — the
  only inspectable-vs-runnable signal that exists, and it is parse-level only.
