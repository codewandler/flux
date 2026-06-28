# How the self-improvement process works

**Status:** implemented · proven end-to-end on terminal-bench (working loop **and** a first kept gain).
**Layer:** L3 crate `flux-eval`. **Owner:** Timo Friedl.

This is the design home for the self-improvement loop (it consolidates the former
`docs/designs/flux-eval.md`): the *process* — what runs, in what order, why each guard exists, how to
drive and observe it — plus where the code lives ([§ Where the code lives](#where-the-code-lives)). The
dated journey and current status are in [STATUS.md](STATUS.md).

## The thesis

> The agent does not get to grade itself, declare victory, or hide the evidence.

A self-improvement loop is only trustworthy if three things hold *at the same time*:

1. **Integrity** — the agent cannot tamper with its own grader.
2. **Validity** — a kept gain has to beat noise, not just a lucky single run.
3. **Target-clarity (anti-overfit)** — the loop must optimize the *harness*, not the *score*.

Everything below exists to enforce those three invariants mechanically, and to leave an auditable
trail so any decision can be re-checked after the fact.

## The loop (one round)

The loop is a pure-DAG flux-flow graph (`examples/improve-tbench.flux`). Each step is a registered op
in `crates/flux-eval/src/ops.rs`. A round runs:

```
 1. eval_run (baseline)      run flux on a task subset, N trials, via a BenchmarkAdapter → report
 2. task("reviewer", …)      a sub-agent reads the report + per-case transcript → JSON improvement list
 3. improvements_aggregate   cluster/rank the suggestions into a candidate list
    ── repeat over candidates ──
 4. task("planner", …)       turn candidates into ≤2 concrete, small, safe code tasks
 5. git_snapshot             capture clean HEAD (refuses a dirty tree → round starts from known state)
 6. change_implement         a "worker" sub-agent implements the tasks (blast radius capped)
 7. guard_protected          restore any tampered grader/harness/CI path (INTEGRITY)
 8. gate_check               cargo build · test · clippy · fmt --check — must be green to proceed
 9. eval_run (candidate)     rebuild the musl binary from candidate source, re-eval same subset
10. score_compare            strict, lexicographic improvement?
       ├─ keep:   git_stage → git_commit → git_tag improve-tbench-<score> → eval_adopt
       └─ revert: git_revert to the snapshot
11. improve_log              append the round's decision + scores to the audit trail
```

The result of a round is always one of: **kept + tagged** (a real commit on the `improve-tbench/<ts>`
branch) or **reverted** (tree restored to the snapshot). Both are recorded.

## The three invariants, concretely

### Integrity — the agent can't grade itself

Sub-agents run through `flux-orchestrate::LocalSpawner` with a fresh scoped tool registry, a headless
`SubAgentApprover`, and no hooks (so they can't be silently permission-blocked). The `task` tool is
**excluded** from sub-agent registries, so a sub-agent cannot spawn its own sub-agents — no unbounded
recursion. That means integrity can't rely on the sub-agent being polite — it's enforced *top-level*: after the worker runs,
`guard_protected` diffs the tree against the `git_snapshot` and **restores any change under PROTECTED
paths** before the candidate is ever graded:

```
crates/flux-eval   bench/   scripts/   .github/   examples/improve-tbench.flux   examples/eval-smoke.flux
```

A worker that edits the grader has its edit reverted, not rewarded. This fired live: in an early run
the reviewer blamed the grader, the planner pointed the worker at `crates/flux-eval`, the worker edited
it — and `guard_protected` rolled it back before the candidate eval, so the tampering changed nothing.
(The reviewer role was then hardened to treat the grader as authoritative; see STATUS.)

### Validity — a kept gain beats noise

- **Multiple trials.** `trials >= 2` (the terminal-bench flow uses 2); the keep decision is on the
  aggregate, not a single run.
- **Partial-credit scoring.** Instead of all-or-nothing pass/fail, the terminal-bench adapter reads
  the per-subtest `parser_results` into `mean_check_pass_rate`. The loop can see and keep progress
  *toward* a full pass (e.g. 5/6 → 6/6), which sharply reduces single-trial binary noise.
- **Strict improvement.** `score_compare` requires the candidate to be *strictly* better under a
  lexicographic order (below) — a tie reverts.

### Target-clarity — improve the harness, not the score

- The **planner** is instructed to target flux's *shipped* harness (its system prompt
  `crates/flux-agent/src/lib.rs`, its built-in tools `crates/flux-tools`, its agent loop
  `crates/flux-agent`) and explicitly **not** to touch `crates/flux-eval`, `bench/`, the loop flows,
  or CI — nor the loop's own `.flux/agents/` / `crates/flux-eval/agents/` roles (editing those changes the
  scaffolding, not the binary under test, so it could never legitimately move the score).
- The **reviewer** works only from the report + the in-container transcript + its own knowledge — it
  never reads or edits the benchmark — so its fixes target real harness friction.
- PROTECTED (above) is the backstop if either sub-agent strays.

## Scoring (`crates/flux-eval/src/score.rs`)

The primary signal is the **weighted full-pass-rate**. Cost is never *blended* into the score; the
comparison is strictly lexicographic, so a faster-but-wronger agent can never outrank a correct one:

1. higher full-pass-rate (tasks passing every trial), then
2. higher **sub-check pass-rate** (`mean_check_pass_rate`, partial credit), then
3. fewer tool-errors, then fewer iterations, then fewer tokens.

The committable scalar baked into a tag is currently `round(pass_rate * 1000)`. This is a known
cosmetic limitation: a candidate that improves only on partial credit (sub-checks) without flipping a
full pass tags as `improve-tbench-0`, because the scalar tracks full-pass-rate, not the check-rate that
actually drove the keep. See STATUS → known gaps.

## The sub-agent roles

Three roles, tracked in `crates/flux-eval/agents/` and seeded into the worktree's `.flux/agents/` by the runner
(the gitignored `.flux/agents/` overrides for local experiments):

- **reviewer** (`tools: []`) — reasons *only* from the eval report + per-case transcript handed to it
  in the prompt. Prioritizes the **dominant friction** (whatever wastes the most agent budget or most
  often blocks the task), ranks by impact, and treats the grader as authoritative. Returns a JSON
  array of `{area, symptom, evidence, severity, suggested_fix}`.
- **planner** (read-only tools) — grounds the candidates in real files and emits ≤2 small, safe,
  independently verifiable tasks `{id, task, files, acceptance}`, targeting the shipped harness only.
- **worker** (read/write/bash) — implements exactly one task, adds/updates a test for any behavioral
  change, keeps the dev-gate green, and never commits/tags/pushes (the harness owns commit/revert).

## Adapters

A `BenchmarkAdapter` trait keeps benchmarks behind one seam:

- **terminal-bench** (the real eval) — shells out to `tb run` with flux as a custom agent
  (`crates/flux-eval/terminal_bench/flux_agent.py`); a static musl flux binary is installed into each task
  container, and `prepare()` rebuilds it from the *candidate* source so the eval measures the worker's
  edits. The in-container session is recorded (asciinema casts), and the tail of `agent.cast` is fed
  back to the reviewer as the per-case transcript. Grading is terminal-bench's own (authoritative).
- **synthetic** (real-model, no Docker) — `LocalAdapter::synthetic()`: short self-contained coding
  riddles with known answers (`crates/flux-eval/assets/synthetic-suite.json`), graded on the produced
  program's stdout. A fast, cheap diagnostic workload — run ad-hoc with `flux eval synthetic`. See
  [synthetic-eval.md](synthetic-eval.md).
- **multi** — `MultiAdapter`: run several adapters behind **one combined score** (ids namespaced
  `<member>:<id>`). The keep-gate `score_compare_multi` refuses a candidate that lifts the combined mean
  while regressing any member, so terminal-bench + synthetic can be graded together without one masking
  the other (`examples/improve-multi.flux`).
- **mock** — an offline, deterministic CI fixture (`LocalAdapter::mock`); used by tests and
  `examples/eval-smoke.flux -m mock` to exercise the loop machinery with no provider or Docker. It is
  **not** a benchmark — just the offline smoke slice.
- **SWE-bench Lite** — a future real adapter behind the same trait.

## Auditability & observation

Nothing in a round is self-reported by the agent; every step leaves a durable trail:

- **`.flux/eval/improve-log.jsonl`** — one record per decision: `decision` (kept / reverted), `reason`
  (`candidate_beat_baseline` / `no_improvement` / `gate_failed`), baseline vs candidate scalar scores,
  the `tag`, and the `guard` / `gate` / `tasks` for that round.
- **`flow.db` RunEvent trace** — every op call with its inputs and results (including the full baseline
  and candidate reports and the `score_compare` verdict). The low-level source of truth.
- **git history + `improve-tbench-<score>` tags** — kept candidates are real commits; reverts restore
  the snapshot and leave no tree change.
- **asciinema recordings** — terminal-bench saves `sessions/agent.cast` (flux working the task) and
  `sessions/tests.cast` (the grader, showing exactly which sub-checks passed/failed).

Observation tooling (in `bench/`, no extra deps):

- **`bench/watch-agent.sh`** — during a run, auto-discovers the live tb task container and streams its
  in-container agent pane (run it in a second shell).
- **`bench/replay-agent.sh`** — after a run, replays any saved `agent.cast` / `tests.cast` (via
  `asciinema play`, or by decoding the cast with python3 — no asciinema needed).

## The loop + the offline smoke

There is one real self-improvement loop — **terminal-bench** (`examples/improve-tbench.flux`, runner
`bench/run-tbench-loop.sh`). For free, provider-less, Docker-less validation of the *flow machinery*
(op wiring, mining, aggregation), there is a mock smoke:
`flux flow run examples/eval-smoke.flux -m mock`. The smoke is a CI fixture, not an evaluation.

(There is no toy "local suite" loop — the former `suites/` + `examples/improve.flux` +
`scripts/improve.sh` were removed; real benchmarks go through adapters, not checked-in TOML tasks.)

## Running it

```sh
bash bench/run-tbench-loop.sh                    # the real self-improvement loop (terminal-bench)
flux flow run examples/eval-smoke.flux -m mock   # offline smoke of the flow machinery (no provider/Docker)
```

`run-tbench-loop.sh` creates an isolated worktree from HEAD on a dedicated branch
(`improve-tbench/<ts>`), seeds the sub-agent roles, builds flux, and runs the flow. `main` is never
touched; the worktree is left in place for inspection (the script prints the discard command). It
refuses to start on a dirty tree, and it puts `HOME` **outside** the worktree so `git_snapshot` always
sees a clean tree — untracked files inside the worktree would make a round refuse to start.

**Requires:** a provider key (`ANTHROPIC_API_KEY` or `flux auth login`). Terminal-bench additionally
needs `tb` on PATH, Docker, and the `x86_64-unknown-linux-musl` target.
**Cost:** sub-agent runs every round; terminal-bench adds musl rebuilds + Docker tasks. A *kept* gain
on hard tasks is not guaranteed — a correct revert is a successful run of the machinery.

### Terminal-bench environment & integration

- **Install:** terminal-bench's `tb` CLI (via `uv`/`pip`); dataset pinned to
  `terminal-bench-core==0.1.1` in the flow.
- **flux as a custom agent:** `tb run --agent-import-path flux_agent:FluxAgent`. The shim
  `crates/flux-eval/terminal_bench/flux_agent.py` (imported via `PYTHONPATH`) copies the static flux binary
  into each task container in `perform_task`; `crates/flux-eval/terminal_bench/flux-setup.sh` then verifies it
  (`flux --version`, emitting `INSTALL_FAIL_STATUS` on failure, which tb treats as an install failure).
- **The binary:** the portable build is the static musl one,
  `target/x86_64-unknown-linux-musl/release/flux` (`cargo build --release --target
  x86_64-unknown-linux-musl`). The adapter's `prepare()` **rebuilds it from candidate source** before
  the candidate eval when `rebuild: true`, so the eval measures the worker's edits — not a stale binary.
- **Results:** tb writes `<output>/<run-id>/results.json`; the adapter reads `is_resolved` plus the
  per-subtest `parser_results` for partial credit, and decodes the tail of `sessions/agent.cast` as the
  transcript fed back to the reviewer.

## Where the code lives

The loop is implemented in the L3 crate `flux-eval`, driven by the flux-flow engine
([`docs/designs/flux-flow.md`](../designs/flux-flow.md)):

- `crates/flux-eval/src/ops.rs` — the registered flux-flow ops: `eval_run`, `eval_scalar`,
  `eval_adopt`, `score_compare`, `guard_protected`, `gate_check`, `git_snapshot` / `git_stage` /
  `git_commit` / `git_tag` / `git_revert`, `change_implement`, `improvements_aggregate`,
  `candidates_empty` / `candidates_advance`, `painpoints_collect`, `improve_log`.
- `crates/flux-eval/src/score.rs` — `SuiteScore`, the lexicographic comparison, partial credit.
- `crates/flux-eval/src/metrics.rs` — `RunResult` / `CaseOutcome` (pass, sub-checks, transcript).
- `crates/flux-eval/src/adapter.rs` — the `BenchmarkAdapter` trait.
- `crates/flux-eval/src/adapters/{local,terminal_bench}.rs` — `local.rs` is the offline `mock`
  fixture; `terminal_bench.rs` is the real adapter, whose `prepare()` rebuilds the static musl binary
  from *candidate* source so the eval measures the worker's edits.
- `crates/flux-eval/terminal_bench/flux_agent.py` — the terminal-bench custom-agent shim (+ `flux-setup.sh`).
- `crates/flux-agent/src/lib.rs` — flux's shipped `DEFAULT_SYSTEM_PROMPT` (a frequent, legitimate
  improvement target); `crates/flux-tools` — its built-in tools.
- Loop flow: `examples/improve-tbench.flux`. Offline smoke: `examples/eval-smoke.flux`.
  Sub-agent roles: `crates/flux-eval/agents/` (tracked) → seeded into `.flux/agents/` (gitignored) by the runner.

The local implementation plan lives under `.flux/plans/` (gitignored).
