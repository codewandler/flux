---
title: 1. Run an adaptive agent safely
description: Watch typed intent, capability-scoped evidence gathering, action-batch approval, and guarded execution.
---

# Run an adaptive agent safely

Your first task is to read the two handbook files and write an onboarding summary. Run it with the
outer-loop machinery visible:

```bash
flux run --show-loop -m sonnet "Read docs/product.md and docs/policies.md, then write a five-bullet onboarding brief to SUMMARY.md. Include the support hours and workspace recovery period."
```

Exact wording varies by model, but the shape is stable. You should first see an intent wait and a
narrow capability result, followed by exploration:

```text
intent…
◆ intent: create a grounded onboarding brief
  capabilities: workspace.read, workspace.write
exploring…
```

The model receives the exact native schemas for the selected operations. Its `read` calls are
low-risk, idempotent evidence gathering, so they execute immediately through the safety envelope.
The `write` call is different: flux captures it as literal `{op, input}` data and freezes it into an
immutable action batch. It has not executed at that point.

Before the write, flux shows the batch and asks for approval. Check that the operation is `write` and
its subject is `SUMMARY.md` inside your tutorial workspace, then approve it.

:::caution
Do not add `--yes` for this exercise. That flag approves every action, including destructive ones.
The batch boundary is part of what you are learning.
:::

After the turn finishes, inspect the file:

```bash
cat SUMMARY.md
```

On PowerShell, use `Get-Content SUMMARY.md`. The wording may differ, but it should mention weekday
support from 09:00–17:00 CET and the 30-day recovery period.

## What just happened

```text
request
  → typed intent
  → capability-scoped native exploration
  → read evidence through the executor
  → host-built action batch
  → approval receipt
  → guarded write
  → final presentation
```

- The **model** interpreted the goal, selected among visible operations, and worded the result.
- The **live registry** supplied operation schemas and the hard capability ceiling.
- The **authored Flux-Lang loop** controlled phase order, bounds, and stopping.
- The **host** constructed the action batch and one-shot receipt; the model could mint neither.
- The **executor** applied authorization, approval, redaction, and workspace-confined IO.
- The session stored intent, evidence, approval, and execution observations for audit.

This is what flux means by **the LLM is not the runtime**. It also explains why Flux-Lang still
matters even though the model does not generate it: deterministic structure belongs around model
judgment, not inside model output.

## Inspect the loop itself

```bash
flux loop show
```

The printed Flux-Lang program is the default outer loop. `flux loop eject` can copy it for editing,
but the file remains inert until you explicitly select it with `--loop` or config.

## Checkpoint

You have used an adaptive turn with reliable operation schemas and a guarded effect boundary. Next
you will author a reusable flow whose structure never has to be inferred.

Continue to [Write a reusable flow](./first-flow.md).
