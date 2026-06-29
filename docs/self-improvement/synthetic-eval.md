# Synthetic eval + `flux eval` + multi-eval

A lightweight, **self-contained** benchmark that complements terminal-bench: short coding riddles with
known answers, run against the real flux binary in an isolated workspace and graded on program output.
It surfaces agent/harness friction (tool-use mistakes, retry loops, wasted iterations) cheaply and fast
— no Docker. It lives behind the same [`BenchmarkAdapter`](../../crates/flux-eval/src/adapter.rs) seam as
`mock` and `terminal-bench`.

## The `synthetic` suite

- Data: `crates/flux-eval/assets/synthetic-suite.json` — a list of [`TaskSpec`](../../crates/flux-eval/src/spec.rs)s,
  each a greenfield task (`setup: empty`) whose prompt asks the agent to write `solution.py`, graded by
  `Criterion::Command { run: "python3 solution.py", stdout_equals: "<answer>" }`.
- Adding a riddle: append a `TaskSpec` to the JSON (id `synthetic/<name>`). Grading is **objective** and
  done outside the agent, so the agent can't "pass" by editing its own grader.
- Prereq: `python3` on `PATH` (the criterion fails cleanly otherwise).
- Grading stdout: `Criterion::Command` gained `stdout_equals` / `stdout_contains` / `stdout_regex`
  (default `None` → exit-code-only, unchanged for existing criteria). `stdout_equals` compares trimmed
  stdout.

## `flux eval` — run a suite ad-hoc

```bash
flux eval <adapter> [--tasks a,b] [--members a,b] [--limit N] [-m <model>] [--trials N] [--report out.md] [--watch]
#   adapters: synthetic | mock | terminal-bench | multi

# the synthetic riddles, watching the agent work live, writing a categorized report:
flux eval synthetic -m openrouter-anthropic/anthropic/claude-sonnet-4.6 --watch --report /tmp/r.md
```

- `--watch` streams each task's agent activity (plan → tool calls → answer) live to the terminal, headed
  by a `── <task-id> ──` banner — the in-eval analogue of `bench/watch-agent.sh`.
- `--report out.md` writes a categorized Markdown report (headline score, per-task table, mined
  pain-points) — the `eval_report_md` op renders the same.
- The same path backs the `eval_run` op, so flows and the CLI drive identical adapters + scoring.

## Multi-eval — grade on several benchmarks at once

The `multi` adapter runs several members behind **one combined score**, with task ids namespaced
`"<member>:<id>"`:

```bash
flux eval multi --members synthetic,mock          # cheap, offline-ish smoke
```

In the improvement loop it lets a candidate be judged on terminal-bench **and** the synthetic riddles
together — broader signal, less overfitting to one benchmark. To stop a gain on one benchmark from
**masking** a regression on another, the combined report carries a per-member score breakdown
(`members: {name → SuiteScore}`) and the keep-gate uses **`score_compare_multi`**: keep iff the combined
candidate is strictly better **and** no member regressed (`pass_rate` & check-rate must not drop).

- Loop flow: `examples/improve-multi.flux` (mirrors `improve-tbench.flux` but `adapter: "multi"` with a
  terminal-bench + synthetic member list, gated on `score_compare_multi`). Both members measure the same
  freshly-rebuilt musl binary (terminal-bench via `flux_binary`, synthetic via the top-level `flux_bin`),
  so the loop scores the worker's edits, not a stale binary.
- Ad-hoc synthetic-only flow: `examples/eval-synthetic.flux`.
- **Synthetic-only self-improvement loop:** `examples/improve-synthetic.flux` (runner
  `bench/run-synthetic-loop.sh`) — the keep/revert loop gated on single-member `score_compare`, at
  **trials = 5**. No Docker and no musl: the candidate's edits are measured through `gate_check`'s
  `target/debug/flux` rebuild, so a round is cheap enough to run trials ≥ 5 for a statistically clean,
  stable-baseline headline gain. This is the recommended vehicle for closing the "clean headline gain"
  gap in [STATUS.md](STATUS.md).
