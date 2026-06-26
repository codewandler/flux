# Self-improvement: status & journey

_Last updated: 2026-06-27._

This is the honest, dated record of where the self-improvement loop stands and how it got here —
including the bugs each live run surfaced, the first kept gain, and the caveats that keep the claims
defensible. For how the loop works, see [DESIGN.md](DESIGN.md).

## TL;DR

- **The loop works end-to-end.** Every stage fires on real Docker / terminal-bench: baseline eval →
  reviewer → aggregate → planner → `git_snapshot` → worker → `guard_protected` → `gate_check` →
  candidate eval → `score_compare` → keep+tag **or** revert → `improve_log`.
- **It has improved the harness for real, once.** On a `fibonacci-server` run, the loop autonomously
  diagnosed a real failure mode, fixed flux's shipped system prompt, measured the candidate beating the
  baseline on partial credit, and **kept + committed + tagged** the change. Details + caveats below.
- **It is auditable.** Every decision lands in `improve-log.jsonl`, the `flow.db` RunEvent trace, git
  tags, and asciinema casts. The agent never grades itself.
- **What's not yet done:** a statistically clean (trials ≥ 3) headline gain, partial-credit-aware tag
  scalars, token/cost capture, and breadth. See [Known gaps](#known-gaps).

## What's proven

| Claim | Status | Evidence |
|---|---|---|
| Machinery runs end-to-end | ✅ proven | multiple live tb runs; every op fires |
| Correct **revert** on a non-improvement | ✅ proven | revert run; grader caught a real flux bug (below) |
| Integrity guard restores tampered grader | ✅ proven (fired live) | `guard_protected` rolled back a worker edit to `crates/flux-eval` |
| Correct **keep + commit + tag** on a real gain | ✅ proven (once) | commit `3c86874`, tag `improve-tbench-0-3c8687…` |
| Statistically clean headline gain (trials ≥ 3) | ⛔ not yet | next chapter |

## The epic's arc (milestones)

What's been built, so a continuer knows the terrain. All landed on `main` unless noted.

- **M1 — crate + offline slice.** `flux-eval` scaffold (spec / adapter / runner / metrics / score),
  the mock adapter, and `flux-cli --output json` + `flow run <file>`.
- **M2 — mining substrate.** flux-flow Usage capture + deterministic pain-point mining
  (`painpoints_collect`). _Token/cost capture is only partly done — see [Known gaps](#known-gaps), #12._
- **M3 — review → aggregate → derive.** Authored `improve.flux` + a fixture test that validates the
  flow.
- **M4 — keep/commit loop.** The loop ops (`git_*`, `gate_check`, `score_compare`) + `scripts/improve.sh`
  + the safety model (dirty-tree refusal, isolated worktree, revert only at top level).
- **M5a — terminal-bench integration.** `tb` install + custom-agent API pin, the Python shim, the
  static musl binary, the `TerminalBenchAdapter`, a one-task Docker smoke, and headroom confirmed
  (flux ~1/3 on moderate tasks → room to improve).
- **M5b — autonomous loop on terminal-bench.** `prepare()` musl rebuild, `improve-tbench.flux`, and
  container-transcript review.
- **Phase A — integrity.** `guard_protected` + PROTECTED paths.
- **Phase B — validity.** Multi-trial eval + strict keep margin.
- **Phase C — signal + audit.** Transcript-fed review + per-round `improve-log.jsonl`. _(Token/cost
  signal deferred to #12.)_
- **Phase D — breadth + docs.** Minimal suite breadth, tracked sub-agent roles, design docs.
- **Phase E — live validation.** The runs in the journey below; bugs found + fixed.
- **Partial credit + trials=2 + the kept-gain proof run** — the most recent work (below).

The open chapters are in [Known gaps](#known-gaps) and [Suggested next steps](#suggested-next-steps).

## The first kept gain (the proof that it improves)

On a `fibonacci-server` round, the loop did the whole thing by itself:

1. **Diagnosed** (from the in-container transcript, not the score): flux detected that a needed runtime
   was absent and/or wrote a server file but never started a listening server, so every grader check
   failed.
2. **Fixed flux's shipped prompt** — `crates/flux-agent/src/lib.rs` `DEFAULT_SYSTEM_PROMPT`, the prompt
   baked into the musl binary that runs inside the container. Two clauses were added to the `bash`
   guidance:
   - verify a runtime exists with `command -v <tool>` before writing files that depend on it, and stop
     + report if it's missing rather than writing files that can't run;
   - for a task needing a persistent server, start it in the background (`nohup … &`) and **confirm the
     port** (`curl --retry --retry-connrefused …` or `ss -tlnp`) before declaring the task complete —
     never write files and exit silently when the server never started.

   Plus a regression test, `default_system_prompt_bash_bullet_has_runtime_checks`.
3. **Measured:** the candidate went from `checks 0%` → **`83%` (both trials)** — visibly, in the cast,
   flux now backgrounds the server, probes the port with `ss`, and pivots runtime when one is absent.
4. **Kept:** `score_compare` adopted it on the partial-credit tiebreaker → `git_commit 3c86874` +
   `git_tag improve-tbench-0` + `eval_adopt`, logged `{decision: kept, reason:
   candidate_beat_baseline}`.

**Where it lives:** branch `improve-tbench/20260626-203839` @ `3c86874`
("improve: adopt candidate (terminal-bench gain)", `crates/flux-agent/src/lib.rs` +43/-1), tag
`improve-tbench-0-3c8687492dc4`. `main` is untouched.

### Honest caveats on that gain

- **The baseline was a noisy-low 0%.** The same flux usually scores ~83% on this task; this run's
  baseline happened to bottom out. So the 0 → 83 *magnitude* is flattered. The defensible claim is not
  "we found 83 points" — it's: **the fix made flux reliably leave a working server (83%, both trials)
  where the un-fixed flux failed entirely (0%, both trials) in the same controlled round.** That is a
  real, transcript-diagnosed behavior improvement, kept by the loop's own rules.
- **The tag reads `-0`.** The scalar baked into a tag is `round(pass_rate*1000)`, and full-pass-rate
  was still 0 (the gain was on sub-checks / partial credit). Cosmetic; tracked in
  [Known gaps](#known-gaps).
- **A single round is not a trend.** "Proven to improve" here means the keep+tag path fired on a
  genuine, autonomously-diagnosed improvement — not that we've demonstrated sustained gains over many
  rounds. That's the next chapter.

## Journey: the runs and what each one taught us

The loop earned trust by being run for real and fixing what broke. Earlier reverts were **not** the
loop misbehaving — each was the machinery working and exposing a bug, which was then fixed on `main`.

1. **Run 1 — wrong layer.** The worker edited `.flux/agents/worker.md` (the loop's own scaffolding),
   which can't change the binary under test. → Fixed by pointing the **planner** at flux's shipped
   harness (`crates/`), not the loop's roles (`d2aa8fa`).
2. **Run 2 — runtime variance.** Candidate quality swung on factors invisible to a score-only reviewer
   (e.g. reaching for an absent runtime). → Fixed by **feeding the in-container transcript** to the
   reviewer and having it prioritize the dominant friction (`3fbe4c8`, `f230255`).
3. **Run 3 — grader-blame + a self-inflicted gate bug.** The reviewer blamed the grader; the planner
   pointed the worker at `crates/flux-eval`; the worker edited it — and `guard_protected` correctly
   rolled it back before grading. Separately, a transcript-code commit went in **un-`fmt`'d**, turning
   the dev-gate red (which means the loop can only ever revert). → Fixed by making the reviewer treat
   the grader as authoritative (`2f49d68`) and by `cargo fmt` (`ba0859e`); and the process rule below
   was adopted.
4. **Run 4 — the kept gain.** Described above.

A separate **correct revert** is worth calling out as a soundness check: a candidate built a
*working-looking* fibonacci server that still reverted, because the grader's hidden
`test_negative_number` caught a real flux bug (it returned `200` instead of `4xx` for `n=-5`). The eval
was valid and the revert was correct — the loop did not reward a plausible-but-wrong solution.

## Bugs the live runs surfaced and fixed (all on `main`, gate green)

- **eval HOME polluted the worktree** → `git_snapshot` saw a dirty tree and crashed. Moved HOME to a
  sibling of the worktree (`62414b2`).
- **reviewer/planner hit a too-low sub-agent iteration cap (15)** → cut off before emitting JSON. Raised
  `LocalSpawner` default to 30 (`d5dff1c`); reviewer set to `tools: []` so it answers from the report.
- **dev-gate was red on HEAD** (`cargo fmt --all --check` failed from earlier un-`fmt`'d commits) → the
  loop could never adopt. `cargo fmt --all` (`8784bab`) + a stale flux-tui test fix (`cc2dc45`).
- **worker edited the wrong layer / reviewer blamed the grader** → fixed via role hardening
  (`d2aa8fa`, `2f49d68`), as in the journey above.

## Hardening landed (all on `main`, gate green)

- **Partial-credit scoring** — the terminal-bench adapter parses `parser_results` into
  `mean_check_pass_rate`, used as the first tiebreaker after full-pass-rate, so the loop sees 5/6 → 6/6
  progress instead of only binary pass/fail (`2199477`).
- **trials = 2** in `improve-tbench.flux`, plus a **per-decision audit log** appended to
  `.flux/eval/improve-log.jsonl` each round (`81fe021`).
- **Tracked sub-agent roles** in `bench/agents/`, seeded into the worktree by the runner (`4930bf9`).
- **Observability** — `bench/watch-agent.sh` (live in-container pane) + `bench/replay-agent.sh`
  (asciinema cast replay, no API) (`7499649`).
- **Design docs** — first written as `docs/designs/flux-eval.md` (`4930bf9`), now consolidated into
  this `docs/self-improvement/` folder.

## Process rules (earned the hard way)

> **Gate before commit.** Run the full **`cargo build · test · clippy · fmt --check`** dev-gate before
> every commit that touches the loop — no subset. An un-`fmt`'d commit turns the gate red, which
> silently disables the loop's ability to ever keep a gain.

> **Never weaken a guard to score a keep.** The loop's value is entirely in its three invariants
> (integrity / validity / target-clarity — see [DESIGN.md](DESIGN.md)). If an improvement only "works"
> by relaxing PROTECTED paths, lowering the trial count, blending cost into the score, or making
> `score_compare` non-strict, it is not an improvement — it is the agent learning to game its grader.
> When extending the loop: **state the invariant a change touches and verify enforcement against the
> code before writing it.** Shallow "make it pass" changes are the failure mode this whole epic exists
> to prevent.

> **A revert is a success, not a failure.** The headline metric for the machinery is "did every stage
> fire and did the decision match the evidence," not "did we keep something." Most rounds should
> revert; a kept gain is the rare, earned outcome.

## Known gaps

- **Partial-credit-aware tag scalar.** `scalar()` is `round(pass_rate*1000)`, so partial-credit-only
  gains tag as `improve-tbench-0`. Make the scalar reflect `mean_check_pass_rate` so tags read
  meaningfully (e.g. `improve-tbench-833`).
- **A statistically clean headline gain.** Run with **trials ≥ 3** on a task whose baseline is stable,
  so the kept gain isn't flattered by a noisy-low baseline.
- **Token/cost capture.** terminal-bench reports 0 tokens for flux; needs flux-flow `Usage` plumbing
  into `RunResult.tokens` (so `mean_tokens` becomes a real tiebreaker). Deferred — flux-flow is under
  concurrent edit. (Task #12.)
- **In-container metrics.** flux's RunEvent trace lives inside the container, so `mean_iterations` /
  `mean_tokens` read 0 for terminal-bench; extract `~/.flux/flow.db` from the container for
  deterministic mining.
- **Breadth.** More local suites and a larger terminal-bench subset; a held-out scoring slice to guard
  against overfitting the chosen tasks. A SWE-bench Lite adapter behind the same trait.

## Suggested next steps

1. **Bring the kept prompt fix to `main`** — `3c86874` (runtime verification + background-server +
   confirm-port) is genuinely good guidance for any shell task and currently lives only on the loop's
   branch.
2. **Fix the tag scalar** to be partial-credit-aware (small, in `score.rs`).
3. **One trials ≥ 3 run** on a stable-baseline task for a clean, defensible headline gain.
4. Optionally, a tracked pre-commit hook to mechanically block un-`fmt`'d commits (enforces the process
   rule above).
