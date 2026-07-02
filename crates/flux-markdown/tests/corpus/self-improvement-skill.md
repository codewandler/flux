---
name: self-improvement
description: >-
  Continuously improve flux's shipped harness (its coding agent: DEFAULT_SYSTEM_PROMPT, built-in
  tools, agent loop) using the CLI self-improvement loop, and prove each gain is real and auditable.
  Use this when asked to improve/evolve the harness, make flux better at benchmarks over time, run a
  self-improvement round or the eval loop, drive improve-tbench.flux / improve.flux /
  bench/run-tbench-loop.sh, mine pain-points and land a harness fix, or continue the self-improvement
  epic.
---

# Self-improvement: continuously improve the flux harness (CLI path)

## The goal (north star)

Make flux's **shipped harness** measurably better over time — and *prove* it. The harness is flux's
own coding agent: its default system prompt, its built-in tools, and its agent loop. The mechanism is
the **CLI self-improvement loop**: flux runs *itself* against real benchmarks via `flux flow run`
(the pure-DAG FlowEngine — "the LLM is not the runtime"), mines the failures, implements a candidate
fix, re-evaluates, and **keeps the change only if it measurably helps with the dev-gate green** —
otherwise it reverts. Every round is auditable; the agent never grades itself.

This skill is the operating playbook. The **authoritative reference is
[`docs/self-improvement/`](../../../docs/self-improvement/)** — read `DESIGN.md` (how it works) and
`STATUS.md` (the living status + dated journey) before doing real work. `STATUS.md` is where you learn
what's already proven and what the open chapters are; **update it after any meaningful run.**

## Read first

1. `docs/self-improvement/STATUS.md` — current state, what's proven, known gaps, next steps, and the
   process rules earned the hard way. **Start here every time.**
2. `docs/self-improvement/DESIGN.md` — the one-round loop, the three invariants, scoring, roles,
   adapters, the two runnable loops + terminal-bench setup, and a code map ("Where the code lives").
3. `AGENTS.md` — the repo contract (dev-gate, layering, safety invariants, commit conventions).

## The one rule that matters most

**Never weaken a guard to score a keep.** The loop's entire value is its three invariants
(integrity / validity / target-clarity). An "improvement" that only works by relaxing PROTECTED paths,
lowering the trial count, blending cost into the score, or making `score_compare` non-strict is not an
improvement — it is the agent learning to game its grader. When you change anything in the loop, state
the invariant it touches and verify enforcement against the code *before* writing it. Corollary: **a
revert is a success**, not a failure — the machinery's job is to make the decision match the evidence.

## How a round works (recap)

`eval (baseline)` → `reviewer` (sub-agent, reads report + transcript) → `aggregate` → `planner` →
`git_snapshot` → `worker` (implements ≤2 small fixes) → `guard_protected` (restores any tampered
PROTECTED path) → `gate_check` (`build·test·clippy·fmt`) → `eval (candidate)` → `score_compare` →
**keep + commit + tag** *or* **revert** → `improve_log`. Full detail in `DESIGN.md`.

## Running it (the CLI path)

One real loop (terminal-bench) + an offline smoke for the flow machinery:

```bash
bash bench/run-tbench-loop.sh                    # THE real self-improvement loop: Docker tasks, tb's grader
flux flow run examples/eval-smoke.flux -m mock   # offline smoke: no provider, no Docker — verify the flow shape
```

(There is no "local suite" loop — the toy `suites/` + `improve.flux` + `scripts/improve.sh` were
removed. Real benchmarks go through adapters; `mock` is only the offline smoke fixture.)

`run-tbench-loop.sh` refuses a dirty tree, creates an isolated worktree on `improve-tbench/<ts>`, seeds
the sub-agent roles, builds flux, runs the flow, and leaves the branch for inspection (it prints the
audit / review / discard commands). **`main` is never touched by the loop.** It needs a provider key
(`ANTHROPIC_API_KEY` or `flux auth login`), plus `tb` on PATH, Docker, and the
`x86_64-unknown-linux-musl` target; it rebuilds the static musl binary from candidate source each round
(`prepare()`) so the eval measures the worker's edits. Setup detail: `DESIGN.md` → "Terminal-bench
environment & integration".

## Procedure when asked to improve the harness

1. **Orient.** Read `STATUS.md`; pick the next open chapter (or the user's specific ask). Confirm the
   dev-gate is green on HEAD first — a red gate means the loop can only ever revert.
2. **Validate the flow first if you changed it** — `flux flow run examples/eval-smoke.flux -m mock`
   exercises the op wiring for free (no provider, no Docker) before you spend on a real run.
3. **Run it** (background it; it's long): `bash bench/run-tbench-loop.sh`. Observe live with
   `bench/watch-agent.sh` (in-container pane) and replay afterward with `bench/replay-agent.sh`
   (asciinema cast, no API). Watching the worker is a stated priority for this project.
4. **Read the verdict** from `<home>/.flux/eval/improve-log.jsonl` (per-round decision/reason/scores)
   and the branch's commits/tags. A kept gain is a real commit tagged `improve-tbench-<score>`; a
   revert restored the snapshot.
5. **Land a kept gain** only after you've sanity-checked it: bring the commit from the loop branch to
   `main` as a normal reviewed change (the loop never auto-pushes to main). Re-run the full dev-gate.
6. **Record it.** Update `docs/self-improvement/STATUS.md` (and memory if you keep one) with what
   happened, honestly — including caveats (noisy baselines, single-round ≠ trend).

## Gotchas earned the hard way (don't re-learn these)

- **Run the FULL dev-gate before every commit that touches the loop** — `cargo build · test · clippy ·
  fmt --check`, no subset. An un-`fmt`'d commit turns the gate red and silently disables the loop's
  ability to keep anything. (This exact mistake was made repeatedly; don't.)
- **HOME must live OUTSIDE the worktree.** The loop writes session/cache state under `$HOME`; anything
  untracked inside the worktree makes `git_snapshot` refuse a dirty tree, so the round can't start. The
  scripts already place it as a sibling / in `.improve-home` — keep it that way.
- **PROTECTED paths are the grader, not improvement targets.** `crates/flux-eval`, `bench/`,
  `scripts/`, `.github/`, `examples/improve-tbench.flux`, and `examples/eval-smoke.flux` are restored
  by `guard_protected` if touched (see the `PROTECTED` list in `crates/flux-eval/src/git.rs`). To
  *improve flux*, change its **shipped harness**: `crates/flux-agent` (`DEFAULT_SYSTEM_PROMPT`),
  `crates/flux-tools`, the agent loop. Do **not** "improve" `.flux/agents/` or `crates/flux-eval/agents/` —
  those are the loop's own scaffolding and editing them can't move the real score.
- **The `task` tool is excluded from sub-agent registries** to prevent unbounded recursion. If you add
  or rewire sub-agents, keep it that way.
- **The tag scalar is cosmetic-buggy:** `round(pass_rate*1000)`, so a partial-credit-only gain tags as
  `improve-tbench-0`. Don't read the tag number as the whole story; read `improve-log.jsonl`.
- **Single runs are noisy.** A 0%→83% headline can be a flattered baseline. For a defensible gain use
  `trials >= 3` on a task with a stable baseline; rely on partial credit (`mean_check_pass_rate`) to
  see sub-pass progress.
- **Concurrent sessions happen in this repo.** Stage only the exact paths you changed; never
  `git add -A` — you may sweep up another session's uncommitted work.

## Extending the loop safely

State the invariant, verify enforcement in code, *then* code (shallow "make it pass" changes are the
failure mode this epic exists to prevent). Keep design + plan in sync: update
`docs/self-improvement/{DESIGN,STATUS}.md` with every change to the loop's behavior. Add a test for any
behavioral change. Open chapters worth pursuing are listed in `STATUS.md` (a clean trials≥3 gain,
partial-credit-aware tag scalar, token/cost capture, in-container metric extraction, breadth + a
held-out slice, a SWE-bench Lite adapter behind the same `BenchmarkAdapter` trait).

## Quick map (full version in DESIGN.md → "Where the code lives")

- Loop ops: `crates/flux-eval/src/ops.rs` · scoring: `score.rs` · adapters: `adapters/{local,terminal_bench}.rs` (`local.rs` = offline mock fixture)
- Flow: `examples/improve-tbench.flux` (the real loop) · offline smoke: `examples/eval-smoke.flux`
- Runner: `bench/run-tbench-loop.sh` · observe: `bench/watch-agent.sh`, `bench/replay-agent.sh`
- Sub-agent roles: `crates/flux-eval/agents/{reviewer,planner,worker}.md` (tracked) → seeded into `.flux/agents/`
- Improvement target: `crates/flux-agent/src/lib.rs` (`DEFAULT_SYSTEM_PROMPT`), `crates/flux-tools`
- Audit: `<home>/.flux/eval/improve-log.jsonl`, git tags `improve-*`, asciinema casts, `flow.db` trace
- Reference example of a real kept gain: commit `3c86874` on branch `improve-tbench/20260626-203839`
