---
id: C-565
title: "Every continued Fleet turn preserves its admitted capability ceiling"
pillar: Core
status: ready
priority: 1
epic: fleet-loop
design: docs/designs/native-board-fleet-cli.md
areas: [flux-cli, flux-capabilities, flux-runtime]
depends_on: [C-566]
note: "dogfood defect — continued writer turns exposed a different operation set than the admitted template"
---

# Every continued Fleet turn preserves its admitted capability ceiling

## Goal

Make a worker's admitted role, mode, capabilities and fences stable across start, message, task,
rework and resume so continuation neither loses required tools nor widens authority.

## Acceptance

- [ ] Failing-first lifecycle fixture admits a writer with a closed operation set and proves each
      continued turn receives the same normalized capabilities, mode, writable root and read roots.
- [ ] Missing required capabilities refuse at admission or delivery with the exact absent names;
      workers do not discover the loss only after spending a model turn.
- [ ] A nested task may narrow its child capabilities but cannot mutate the parent's admission. A
      follow-up cannot add tools, paths, network or process authority absent a new explicit admission.
- [ ] Durable status and turn receipts record the admitted/effective capability-set digest without
      embedding prompts, secrets or the full operation catalogue.
- [ ] Template reload, restart, rework and resume behavior is explicit and tested; an existing
      worker does not silently adopt a wider edited template.
- [ ] The three-repository, five-writer dogfood run completes with every writer retaining the
      capabilities its story requires.

## Notes

- Filed as the admitted-capability follow-up required by C-560.
