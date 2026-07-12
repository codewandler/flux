---
title: 1. Run an agent safely
description: Preview a model-generated Flux-Lang plan, execute it through the guarded runtime, and review a write approval.
---

# Run an agent safely

You have two short handbook files. Your first task is to ask the coding agent to read them and write
an onboarding summary.

Make sure your terminal is still in the tutorial workspace:

```bash
pwd
flux auth status
```

On PowerShell, use `Get-Location` instead of `pwd` if it is not available.

## Preview the plan

Ask the model to gather any read-only context it needs and preview the remaining plan without
executing that returned plan:

```bash
flux plan -m sonnet -o yaml "Read docs/product.md and docs/policies.md, then write a five-bullet onboarding brief to SUMMARY.md. Include the support hours and workspace recovery period."
```

Your exact plan will vary by model. Plan mode may run up to three small, bounded **read-only gather**
rounds so the final proposal is grounded. If that happens, the two `read` operations have already
run and the printed plan may contain only the pending `write` to `SUMMARY.md`. A model that settles
without a gather round may instead print the reads and write together.

In both cases, the important guarantee is the same: the **printed plan itself is left unexecuted**.
Plan mode never auto-executes its returned mutation plan, so `SUMMARY.md` does not exist yet.

This is the first important boundary in flux:

```text
your request -> model -> bounded read-only gather (if needed) -> typed pending plan
                                                           (stops here)
```

The model can decide what to propose. The runtime alone performs the bounded gather and stops before
the returned plan; the model cannot perform IO itself.

## Run the task

Now run the same request:

```bash
flux run -m sonnet "Read docs/product.md and docs/policies.md, then write a five-bullet onboarding brief to SUMMARY.md. Include the support hours and workspace recovery period."
```

flux executes the accepted plan one operation at a time. Reads are normally pre-authorized. Before
a write or command that needs approval, flux shows what operation wants to run and which path it
affects. Confirm the `SUMMARY.md` write after checking that its subject is inside your tutorial
workspace.

:::caution
Do not add `--yes` for this exercise. That flag approves every step, including destructive ones. The
approval pause is part of what you are learning.
:::

When the run finishes, open `SUMMARY.md`, or print it on a POSIX shell:

```bash
cat SUMMARY.md
```

The wording may differ, but the brief should mention weekday support from 09:00–17:00 CET and the
30-day recovery period.

## What just happened

The complete path was:

```text
request -> model -> typed plan -> authorization -> approval -> guarded IO
```

- The **model** translated your goal into a plan.
- The **runtime** validated and executed that plan.
- Each **operation** crossed the same policy and approval boundary.
- The **workspace guard** confined file access to permitted roots.
- The run and its evidence were recorded as a session; `flux sessions` lists recent sessions.

This division is what flux means by **the LLM is not the runtime**.

## Checkpoint

You have used natural language to produce an inspectable, guarded plan. Next you will remove the
planning step and write a reusable plan directly.

Continue to [Write a reusable flow](./first-flow.md).
