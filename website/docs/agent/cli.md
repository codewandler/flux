---
title: CLI
---

# CLI

The CLI is the reference flux surface. It runs agent turns, executes stored flows, hosts app programs,
and exposes development utilities.

Common paths:

```bash
flux run "fix the failing test"
flux flow run path/to/flow.flux
flux app run path/to/app.flux
```

During a turn, the model has no directly callable tools. It emits a plan, and each operation in that
plan is dispatched by the runtime. Approval prompts appear when policy, risk, or permission rules
require human confirmation.

## Agent loop visibility

The default agent loop is itself a Flux-Lang flow. Normal runs hide this machinery, but you can inspect
it when debugging:

```bash
flux run --show-loop "summarize the docs"
flux loop show
flux loop eject
```

Use the repository's internal agent-loop guide for contributor-level details.
