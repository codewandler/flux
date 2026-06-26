# Design: flux-eval (the self-improvement loop)

**Status:** Implemented · validated end-to-end on terminal-bench · **Layer:** L3 crate `flux-eval` ·
**Owner:** Timo Friedl

`flux-eval` is the harness that lets flux **improve its own harness over time** and prove it. It runs
flux against real coding/shell benchmarks, mines the failures, derives + implements a candidate fix,
re-evaluates, and **keeps the change only if it measurably helps** (and the dev-gate stays green) —
otherwise it reverts. The whole loop is authored as a flux-flow graph (`examples/improve-tbench.flux`,
`examples/improve.flux`) and executed by `flux flow run`. See [flux-flow.md](flux-flow.md) for the
engine; the local implementation plan lives under `.flux/plans/` (gitignored).

## 1. Thesis

> The agent does not get to grade itself, declare victory, or hide the evidence.

Self-improvement is only trustworthy if three things hold simultaneously: the agent **cannot tamper
with its own grader** (integrity), a kept gain must **beat noise** (validity), and the loop must
optimize the **harness, not the score** (target-clarity / anti-overfit). Everything below exists to
enforce those three invariants mechanically, and to make every step **auditable** after the fact.

## 2. The loop (one round)

Authored as a pure-DAG flow; each step is a registered op (`flux-eval/src/ops.rs`):

1. `eval_run` — baseline: run flux on a task subset (N trials) via a `BenchmarkAdapter`; produce a
   report (pass-rate, partial-credit, per-case detail).
2. `task("reviewer", …)` — an external sub-agent reads the report and proposes harness improvements
   (JSON). For local runs, `painpoints_collect` also mines the RunEvent trace deterministically.
3. `improvements_aggregate` → `candidates` — cluster/rank into a candidate list.
4. `git_snapshot` — capture clean `HEAD` (refuses a dirty tree, so a round starts from a known state).
5. `task("planner", …)` → `change_implement` — turn candidates into concrete tasks; a `worker`
   sub-agent implements them (blast-radius capped by `limit`).
6. `guard_protected` — restore any tampered grader/suite/CI path (integrity; see §4).
7. `gate_check` — `cargo build · test · clippy · fmt --check`; must be green to proceed.
8. `eval_run` — candidate: re-evaluate the changed flux on the same subset.
9. `score_compare` → keep or revert:
   - **keep:** `git_stage` + `git_commit` + `git_tag improve-<score>` + `eval_adopt` (candidate
     becomes the new baseline).
   - **revert:** `git_revert` to the snapshot.
10. `improve_log` — append the round's decision + scores to the audit trail (§5).

## 3. Adapters — `BenchmarkAdapter`

A trait (`adapter.rs`) so benchmarks plug in behind one seam:

- **local** (`adapters/local.rs`) — task specs in `suites/`, graded by `cargo test` / file checks;
  in-process via flux-flow, so the RunEvent trace is available for deterministic mining.
- **terminal-bench** (`adapters/terminal_bench.rs`) — shells out to `tb run` with flux as a custom
  agent (`bench/terminal_bench/flux_agent.py`); a static musl flux binary is installed into each task
  container. `prepare()` rebuilds that binary from the *candidate* source so the eval measures the
  worker's edits. Grading is terminal-bench's own (authoritative).
- **mock** — offline, deterministic; used by tests and `examples/eval-smoke.flux -m mock`.

SWE-bench Lite is the next adapter behind the same trait.

## 4. Invariants and how they're enforced

- **Integrity — the agent can't grade itself.** Sub-agents (`flux-orchestrate::LocalSpawner`) get a
  fresh scoped tool registry, a headless `SubAgentApprover`, and no hooks, so they can't be
  permission-blocked. Integrity is therefore enforced *top-level*: `guard_protected` diffs against the
  snapshot and restores any change under PROTECTED paths (`crates/flux-eval`, `suites/`, `bench/`,
  `scripts/`, `.github/`, the loop flows) before the candidate is graded. A worker that edits the
  grader has its edit reverted, not rewarded.
- **Validity — a kept gain beats noise.** Multi-trial eval (`trials >= 2`) with the keep decision on
  the aggregate; partial-credit scoring (§6) reduces binary-pass noise; `score_compare` requires a
  *strict* improvement under a lexicographic order.
- **Target-clarity — improve the harness, not the score.** The planner is told not to touch the eval
  crate / suites / CI; PROTECTED enforces it. The reviewer works from the report + its knowledge, so
  fixes target real harness friction (e.g. a tool's description, a missing capability, a loop
  inefficiency), not the benchmark.

## 5. Auditability & observation

Every round leaves a durable, inspectable trail — nothing is self-reported by the agent:

- **`.flux/eval/improve-log.jsonl`** — one record per decision: `decision` (kept / reverted),
  `reason` (candidate_beat_baseline / no_improvement / gate_failed), baseline vs candidate scalar
  scores, the `tag`, `guard`, `gate`, and `tasks`.
- **`flow.db` RunEvent trace** — every op call, its inputs, and results (incl. the full baseline +
  candidate reports and the `score_compare` verdict). The low-level source of truth.
- **git history + `improve-<score>` tags** — kept candidates are real commits; reverts leave no trace
  in the tree (snapshot restored).
- **asciinema recordings** — terminal-bench records every session: `sessions/agent.cast` (flux
  working the task) and `sessions/tests.cast` (the grader). The grader cast shows exactly which
  sub-checks passed/failed.

Observation tooling (`bench/`, zero extra deps):

- **`bench/watch-agent.sh`** — during a run, auto-discovers the live tb task container and streams its
  in-container agent pane (run it in a second shell).
- **`bench/replay-agent.sh`** — after a run, replays any saved `agent.cast` / `tests.cast` (via
  `asciinema play`, or by decoding the cast with python3 — no asciinema needed).

## 6. Scoring (`score.rs`)

Primary signal is the **weighted full-pass-rate**. Cost is never blended in; comparison is strictly
lexicographic so a faster-but-wronger agent can't outrank a correct one:

1. higher full-pass-rate (tasks passing every trial), then
2. higher **sub-check pass-rate** (partial credit — `mean_check_pass_rate`), then
3. fewer tool-errors, then fewer iterations, then fewer tokens.

Partial credit comes from terminal-bench's per-subtest `parser_results` (e.g. a server passing 5/6,
failing only `test_negative_number`): the loop can see and keep progress *toward* a full pass instead
of only all-or-nothing, which sharply reduces single-trial noise. Binary-only adapters fall back to
pass/fail, so their behavior is unchanged. The committable scalar for a tag is `round(pass_rate*1000)`.

## 7. Running it

`bash bench/run-tbench-loop.sh` — creates an isolated worktree from HEAD, seeds the tracked sub-agent
roles (`bench/agents/{reviewer,planner,worker}.md`) into the worktree's `.flux/agents/`, builds flux,
and runs `examples/improve-tbench.flux`. The home dir is a *sibling* of the worktree (never inside it,
so `git_snapshot` sees a clean tree). Requires `tb` on PATH, Docker, the musl target, and a provider
key. `main` is never touched — everything happens on `improve-tbench/<ts>`.

## 8. Validation status

The loop is proven end-to-end on terminal-bench: every stage fires, and a round ends in a recorded
keep+tag or a correct revert. The first live runs surfaced + fixed four real bugs — eval HOME
polluting the worktree, the reviewer/planner exhausting a too-low sub-agent iteration cap (15→30), and
a workspace-wide `cargo fmt` drift that made the dev-gate red (so the loop could never adopt). A
candidate that built a *working-looking* fibonacci server still (correctly) reverted: the grader's
hidden `test_negative_number` caught a real bug (200 instead of 4xx for `n=-5`), confirming the eval
is sound. The keep+tag branch is the only path not yet exercised on a real gain — it needs a
grader-confirmed improvement, which is what partial credit + `trials>=2` are designed to surface.

## 9. Known gaps / future

- **Token/cost capture** — terminal-bench reports 0 tokens for flux; needs flux-flow `Usage` plumbing
  into `RunResult.tokens` (deferred; coordinate with the concurrent flux-flow work).
- **In-container metrics** — flux's RunEvent trace lives inside the container, so `mean_iterations` /
  `mean_tokens` read 0 for terminal-bench; extract `~/.flux/flow.db` from the container for
  deterministic mining.
- **Breadth** — more local suites and a larger terminal-bench subset; a held-out scoring slice.
- **SWE-bench Lite adapter** behind the same trait.
