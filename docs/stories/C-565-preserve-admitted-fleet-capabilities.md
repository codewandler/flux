---
id: C-565
title: "Every continued Fleet turn preserves its admitted capability ceiling"
pillar: Core
status: in-progress
priority: 1
epic: fleet-loop
design: docs/designs/native-board-fleet-cli.md
areas: [flux-cli, flux-capabilities, flux-runtime]
depends_on: [C-566, C-567]
note: "dogfood defect — continued writer turns exposed a different operation set than the admitted template"
---

# Every continued Fleet turn preserves its admitted capability ceiling

## Goal

Make a worker's admitted role, mode, capabilities and fences stable across start, message, task,
rework and resume so continuation neither loses required tools nor widens authority.

## Acceptance

- [x] Failing-first lifecycle fixture admits a writer with a closed operation set and proves each
      continued turn receives the same normalized capabilities, mode, writable root and read roots.
- [x] Missing required capabilities refuse at admission or delivery with the exact absent names;
      workers do not discover the loss only after spending a model turn.
- [x] A nested task may narrow its child capabilities but cannot mutate the parent's admission. A
      follow-up cannot add tools, paths, network or process authority absent a new explicit admission.
- [x] Durable status and turn receipts record the admitted/effective capability-set digest without
      embedding prompts, secrets or the full operation catalogue.
- [x] Template reload, restart, rework and resume behavior is explicit and tested; an existing
      worker does not silently adopt a wider edited template.
- [ ] The three-repository, five-writer dogfood run completes with every writer retaining the
      capabilities its story requires.

## Notes

- Filed as the admitted-capability follow-up required by C-560.

## Evidence

- The failing-first `story_worker_launch_argv_enforces_an_operation_ceiling` fixture recorded a
  worker argv with no host-owned ceiling. The implementation now snapshots normalized named
  bundles and expands them to repeated hidden `--operation` arguments under one
  `--operation-ceiling`; the runtime validates every named operation before a model turn and keeps
  the resulting executor scope alive for the full fresh or continued turn.
- The real local two-story Fleet integration starts distinct fresh workers, widens the source
  template and instruction file, then continues, status-inspects, resumes and reworks the first
  worker in separate CLI processes. Every receipt retains session `s_1`, its original worker
  contract and exact `flux.fleet-capability-set/v1` digest; status retains the original normalized
  capabilities, write mode, writable worktree and empty sibling read-root set.
- Focused runtime coverage passes
  `nested_scope_narrows_and_never_widens` and
  `task_tool_forwards_the_contexts_active_cap_scope_to_the_spawner`, proving a nested task inherits
  the active parent scope and cannot add an operation the parent lacks. Missing writer and
  read-only capabilities fail at admission with sorted exact names.
- The final three-repository, five-writer dogfood acceptance remains open until the gated binary is
  installed and used for the scheduled cross-repository wave.
