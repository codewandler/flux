---
title: Evaluation and improvement
description: "Measure Flux behavior with repeatable eval adapters, inspect reports, and understand the evidence-driven improvement loop."
---

# Evaluation and improvement

The Improvement pillar closes the loop between agent behavior and engineering changes: run a fixed
task set, record evidence and scores, change one candidate, then keep it only when repeated evaluation
shows a real gain without unacceptable regressions.

## Run an evaluation

```bash
flux eval mock                                      # offline wiring/CI fixture
flux eval synthetic -m sonnet --trials 3 --watch
flux eval terminal-bench -m sonnet --trials 3 --report report.md
flux eval multi --members synthetic,terminal-bench --trials 3
```

| Adapter | Use |
|---|---|
| `mock` | Provider-free smoke test of the eval plumbing. It is not a quality benchmark. |
| `synthetic` | Short real-model coding riddles; fast enough for iteration. |
| `terminal-bench` | Docker-backed terminal tasks and authoritative task graders. Requires the `tb` tooling and container runtime. |
| `multi` | Combine named members behind one score while retaining member results. |

Use `--tasks a,b` to select cases, `--limit N` for a quick subset, `--watch` for live activity, and
`--report out.md` for a categorized report. A single model trial is useful for debugging but noisy
evidence; use at least three trials for a keep/reject comparison.

## Strict review

`flux review --files …` is a separate read-only quality protocol: built-in reviewer roles inspect
the files and `review.aggregate` produces stable Markdown or JSON. `--fail-on high` makes it useful
as a gate.

## The repository improvement loop

Flux's own loop adds candidate generation, isolated evaluation, comparison, and an auditable
keep/reject record around the CLI adapters. The implementation is intentionally kept in the
repository so changes to the harness are reviewed like any other product change. Start with the
[self-improvement overview](https://github.com/codewandler/flux/tree/main/docs/self-improvement)
when working on Flux itself.

## Related docs

- [Usage & cost](./cost.md) — interpret model spend alongside scores.
- [Time Machine](./time-machine.md) — replay and compare individual runs.
- [CLI](./cli.md) — `flux eval` and `flux review` options.
