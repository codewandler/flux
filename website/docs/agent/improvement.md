---
title: Evaluation and improvement
description: "Where harness benchmarking lives (flux-bench), what `flux eval` is for now, and the honest status of the repository self-improvement loop."
---

# Evaluation and improvement

Two different things used to share this page. They have been separated, because only one of them
moved:

- **Benchmarking a harness** — measuring whether one build of flux is better than another. That is
  [flux-bench](https://github.com/codewandler/flux-bench)'s job now, in its own repository.
- **The repository self-improvement loop** — flux editing its own harness under a keep-or-revert
  gate. That stays here, because it edits *this* tree. It is real, shipped, runnable, and
  [on hold](#the-repository-self-improvement-loop).

## Benchmarking the harness: flux-bench

**flux-bench measures a harness — the system prompt, the built-in tools, the agent loop — rather
than a model.** It runs the *shipped* flux binary against a curated corpus with the model held fixed
and verified fixed, so the harness is the only variable. Two properties make its answers worth
having: it **measures its own noise floor** by running a binary against itself, and reports any
difference inside that floor as `INCONCLUSIVE` rather than as a win; and it grades what an agent
**declines** to do, because a case can forbid an action and match it against the tool call's *input
arguments* on flux's `--stream-json` wire. That turns "did not hijack the user's audio device" into
a measurable outcome instead of a code-review note.

→ **[codewandler/flux-bench](https://github.com/codewandler/flux-bench)** — the supported answer to
"how do I measure this agent".

It has no documentation site by design: its
[`README.md`](https://github.com/codewandler/flux-bench/blob/main/README.md) and
[`docs/`](https://github.com/codewandler/flux-bench/tree/main/docs) are the reference, and
[`docs/from-flux-eval.md`](https://github.com/codewandler/flux-bench/blob/main/docs/from-flux-eval.md)
carries the measurement practice that used to live on this page — how many trials a claim needs, when
a task is unscoreable, and how to audit a score back to the run that produced it.

Because flux-bench runs the binary flux *ships*, it follows flux releases and not the other way
round. Nothing in flux depends on it.

## `flux eval` — still shipped, still supported

`flux eval` is unchanged: same adapters, same flags, same exit codes. It is not deprecated and
nothing about it is going away — it simply is not the answer to "benchmark my harness" any more.
What it is for:

- **The scoring engine the [self-improvement loop](#the-repository-self-improvement-loop) drives.**
  The loop calls the same suites through the `eval_run` op, so the CLI is how you reproduce or debug
  a round by hand.
- **An offline CI fixture.** `flux eval mock` needs no network and no credentials, and proves the
  eval plumbing works.

```bash
flux eval mock                                      # offline wiring/CI fixture
flux eval synthetic -m sonnet --trials 3 --watch
flux eval terminal-bench -m sonnet --trials 3 --report report.md
flux eval multi --members synthetic,terminal-bench --trials 3
```

Every adapter and flag is documented by `flux eval --help`, which cannot drift from the binary; that
help also names flux-bench. Running the command prints the same pointer **on stderr**, so a caller
parsing the summary on stdout or diffing a `--report` file is unaffected.

One calibration result is worth keeping in view: `synthetic` is **saturated** for current frontier
models — a 2026-07-02 run scored 1000/1000 twice, with two different models. It remains a useful
regression floor and is a poor vehicle for demonstrating a gain.

## The repository self-improvement loop

:::note Status
The Improvement pillar is **de-prioritized and on hold** (since 2026-07-06 — a project priority
call). Everything in this section is real, shipped, and runnable. What is **not** proven is the
pillar's headline claim: a repeatable, grader-confirmed gain at **trials ≥ 3**. The autonomous loop
has been driven end-to-end and has correctly *reverted* non-improvements; it has not yet
demonstrated a statistically clean win. Do not build a plan around the loop reliably improving an
agent for you.

The dated, per-round record is
[`docs/self-improvement/STATUS.md`](https://github.com/codewandler/flux/blob/main/docs/self-improvement/STATUS.md).
:::

The loop is authored as a Flux-Lang flow, not as Rust: baseline eval → review the failures → derive
a candidate harness fix → a `worker` sub-agent implements it → restore the protected paths → run the
dev gate → re-evaluate → keep and tag **iff** strictly better, else revert.

The ops that flow calls — `eval_run`, `score_compare_multi`, `painpoints_collect`,
`guard_protected`, `git_reset`, and the rest — are documented once, with their arguments, in
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

### Where a round's evidence lands

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

### Why the loop lives in this repository

The loop edits flux's own harness, so it is reviewed like any other product change rather than
hidden behind a service. Moving it to flux-bench would be the wrong direction *and* would break the
property flux-bench is built around: an instrument must not be writable by the thing it measures.
Two properties do the load-bearing work here:

- **The agent never grades itself.** Scores come from the benchmark's own graders, and
  `score_compare_multi` additionally requires that no member benchmark regressed — a single combined
  score can hide a regression.
- **`guard_protected` is the anti-cheat step.** After each worker run it restores the grader, the
  suite, the loop flow, and the CI config to the round's snapshot, so a round cannot raise its own
  score by editing what measures it. `git_reset` carries the `Destructive` risk tier and is
  approval-gated.

Those make a *reported* gain trustworthy. They do not make a gain happen — which is exactly the gap
the status note above describes.

## Strict review

`flux review --files …` is a separate, read-only quality protocol and is **not** part of the hold.
Built-in reviewer roles inspect the named files and `review.aggregate` produces stable Markdown
(`--format md`, the default) or the raw `ReviewReport` (`--format json`). `--fail-on high` exits
non-zero when any finding meets that severity, which makes it usable as a CI gate. The reviewer
roles and the `strict_review` flow are embedded in the binary, so it works in any repository.

## Related docs

- [flux-bench](https://github.com/codewandler/flux-bench) — the supported harness benchmark.
- [Operations → Improvement loop](../language/ops.md#improvement-loop) — every eval, mining, and round-guard op with its arguments.
- [Time Machine](./time-machine.md) — replay, fork, and diff an individual eval session.
- [Usage & cost](./cost.md) — price a run alongside its score.
- [CLI](./cli.md) — `flux eval` and `flux review` in the full command surface.
- [Deterministic Agent Lab](../sdk/agent-lab.md) — the SDK-side counterfactual and golden-fixture tooling for embedded agents.
