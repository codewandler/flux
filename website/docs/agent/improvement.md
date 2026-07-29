---
title: Evaluation and improvement
description: "What flux eval measures, where its evidence lands, and the honest status of the repository self-improvement loop."
---

# Evaluation and improvement

:::note Status
The Improvement pillar is **de-prioritized and on hold** (since 2026-07-06 — a project priority
call). Everything described on this page is real, shipped, and runnable. What is **not** proven is
the pillar's headline claim: a repeatable, grader-confirmed gain at **trials ≥ 3**. The autonomous
loop has been driven end-to-end and has correctly *reverted* non-improvements; it has not yet
demonstrated a statistically clean win.

Use `flux eval` as a measurement and audit harness. Do not build a plan around the loop reliably
improving an agent for you. The dated, per-round record is
[`docs/self-improvement/STATUS.md`](https://github.com/codewandler/flux/blob/main/docs/self-improvement/STATUS.md).
:::

## `flux eval` — run a benchmark suite

`flux eval` runs a fixed task set against the flux binary and prints a scored summary. Each case
runs the **real agent** in an isolated temporary workspace with its own `HOME`, so cases cannot see
each other, your shell environment, or your session history.

```bash
flux eval mock                                      # offline wiring/CI fixture
flux eval synthetic -m sonnet --trials 3 --watch
flux eval terminal-bench -m sonnet --trials 3 --report report.md
flux eval multi --members synthetic,terminal-bench --trials 3
```

### Adapters

| Adapter | What it runs | Requirements |
|---|---|---|
| `mock` | Offline CI fixture that drives `-m mock`. Proves the eval plumbing, not model quality. | none |
| `synthetic` | Short real-model coding riddles; fast enough to iterate on. | a provider key |
| `terminal-bench` | Docker-backed terminal tasks graded by the benchmark's own graders. | `tb` on `PATH`, Docker |
| `multi` | Several members behind one combined score, member results retained. | those of each member |

`synthetic` is **saturated** for current frontier models — a 2026-07-02 calibration scored 1000/1000
twice, with two different models. That makes it a useful regression floor and a poor vehicle for
demonstrating a gain; a claimed improvement has to come from `terminal-bench`.

### Flags

| Flag | Effect |
|---|---|
| `-m, --model <spec>` | Model the suite's agent runs (`-m mock`, `-m openrouter/anthropic/claude-sonnet-4.6`, …). |
| `--tasks a,b` | Restrict to these task ids. |
| `--members a,b` | For `multi` only: the member adapters to combine. Checked at startup. |
| `--limit N` | Cap the number of tasks (`0` = all). |
| `--trials N` | Trials per task (default `1`). |
| `--report <path>` | Write a categorized Markdown report (headline score, per-task table). |
| `--watch` | Stream each task's agent activity to the terminal live. |

A single trial is fine for debugging and is **not** evidence: model noise on a small suite easily
swamps the effect you are looking for. Use `--trials 3` or more before comparing two runs, and treat
a task whose baseline swings between trials as unscoreable rather than as a signal.

## Where the evidence lands

- **The report** goes wherever `--report` points. It is ordinary Markdown — commit it, diff it,
  paste it into a review.
- **Per-case sessions** are real flux sessions inside each case's isolated `HOME`
  (`<workdir>/.home/.flux/events.db`). The report carries a reference to each one, which is what
  makes a score auditable back to the run trace that produced it — the `eval_sessions` op extracts
  those references, and `sessions_digest` / `painpoints_collect` read them.
- **Improvement rounds** append one record per keep/revert decision to
  `.flux/eval/improve-log.jsonl` under the running `HOME`, and tag kept rounds in git.

Because a case's session is an ordinary session, every read-back surface applies to it: replay it,
diff two runs, or price it. See [Time Machine](./time-machine.md) and [Usage & cost](./cost.md).

## Strict review

`flux review --files …` is a separate, read-only quality protocol and is **not** part of the hold.
Built-in reviewer roles inspect the named files and `review.aggregate` produces stable Markdown
(`--format md`, the default) or the raw `ReviewReport` (`--format json`). `--fail-on high` exits
non-zero when any finding meets that severity, which makes it usable as a CI gate. The reviewer
roles and the `strict_review` flow are embedded in the binary, so it works in any repository.

## The repository improvement loop

The loop is authored as a Flux-Lang flow, not as Rust: baseline eval → review the failures → derive
a candidate harness fix → a `worker` sub-agent implements it → restore the protected paths → run the
dev gate → re-evaluate → keep and tag **iff** strictly better, else revert.

The ops that flow calls — `eval_run`, `score_compare_multi`, `painpoints_collect`,
`guard_protected`, `git_revert`, and the rest — are documented once, with their arguments, in
[Operations → Improvement loop](../language/ops.md#improvement-loop). They are registered in every
session, so any flow can call them.

What actually exists in the repository:

| Path | What it is |
|---|---|
| [`examples/improve-tbench.flux`](https://github.com/codewandler/flux/blob/main/examples/improve-tbench.flux) | The real loop, graded on terminal-bench. |
| [`examples/improve-synthetic.flux`](https://github.com/codewandler/flux/blob/main/examples/improve-synthetic.flux) | The same shape against the cheaper synthetic suite. |
| [`examples/improve-multi.flux`](https://github.com/codewandler/flux/blob/main/examples/improve-multi.flux) | Multi-benchmark variant with a per-member regression guard. |
| [`examples/eval-smoke.flux`](https://github.com/codewandler/flux/blob/main/examples/eval-smoke.flux) | Free, offline smoke of the flow machinery: `flux flow run examples/eval-smoke.flux -m mock`. |
| [`bench/run-tbench-loop.sh`](https://github.com/codewandler/flux/blob/main/bench/run-tbench-loop.sh) | Driver for the real loop. |
| [`bench/run-synthetic-loop.sh`](https://github.com/codewandler/flux/blob/main/bench/run-synthetic-loop.sh) | Driver for the synthetic loop. |

There is no single `improve.flux`; the loop is always one of the three named flows above.

`run-tbench-loop.sh` creates a **disposable git worktree** from `HEAD` on an `improve-tbench/<ts>`
branch, builds flux there, and runs the loop with a sibling `HOME` outside the worktree — `main` is
never touched, and the script prints the exact commands to audit, review, or discard the run. It is
expensive (musl rebuilds, Docker tasks, sub-agent runs), and a kept gain is not guaranteed: the loop
reverting is the expected outcome, not a failure of the script.

### Why it lives in this repository

The loop edits flux's own harness, so it is reviewed like any other product change rather than
hidden behind a service. Two properties do the load-bearing work:

- **The agent never grades itself.** Scores come from the benchmark's own graders, and
  `score_compare_multi` additionally requires that no member benchmark regressed — a single combined
  score can hide a regression.
- **`guard_protected` is the anti-cheat step.** After each worker run it restores the grader, the
  suite, the loop flow, and the CI config to the round's snapshot, so a round cannot raise its own
  score by editing what measures it. `git_revert` carries the `Destructive` risk tier and is
  approval-gated.

Those make a *reported* gain trustworthy. They do not make a gain happen — which is exactly the gap
the status note above describes.

## Related docs

- [Operations → Improvement loop](../language/ops.md#improvement-loop) — every eval, mining, and round-guard op with its arguments.
- [Time Machine](./time-machine.md) — replay, fork, and diff an individual eval session.
- [Usage & cost](./cost.md) — price a run alongside its score.
- [CLI](./cli.md) — `flux eval` and `flux review` in the full command surface.
- [Deterministic Agent Lab](../sdk/agent-lab.md) — the SDK-side counterfactual and golden-fixture tooling for embedded agents.
