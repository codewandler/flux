# flux — observed problems & improvements

_Produced by the **A2A self-observation loop**: short, self-contained coding questions are sent
to flux over the A2A protocol in a wiped temp workspace; each live session is observed, then mined
from `~/.flux/events.db`; findings are triaged below._

Each run solves a question with a known answer, so **correctness is objective** (`pass`/`fail`/`error`).
The structured event log gives the rest of the signal: planning attempts & compile errors
(`plan_attempted`), per-step tool failures (`run` → `step_failed`), loop rounds (`run_plan`), and the
final outcome (`turn_ended`).

- **Model under observation:** `openrouter-anthropic/anthropic/claude-sonnet-4.6` (Sonnet via OpenRouter; `flux serve`)
- **Sample:** 30 runs, sessions `s_133`–`s_162`, over 16 curated questions (most run twice).
- **Harness:** now first-class in `flux-eval` — the `synthetic` benchmark suite
  (`crates/flux-eval/assets/synthetic-suite.json`), run with **`flux eval synthetic [--watch] [--report out.md]`**.
  See [`docs/self-improvement/synthetic-eval.md`](self-improvement/synthetic-eval.md). _(The original ad-hoc
  `scratchpad/a2a-loop/` harness this report was first produced with is superseded.)_
- **Status:** all four findings **fixed and verified** on branch `fix/agent-loop-robustness` — see Resolution below.

---

## Aggregate summary — 30 runs

**Headline**
- **Correctness: 30/30 produced the right answer on disk** — but the 8 catastrophic runs (below) were *never
  verified by the agent itself*; they passed only because `write` left a correct file that the harness's
  out-of-band `verify_cmd` ran, while the agent exited with a garbage answer. **Agent-confirmed success is 22/30.**
- **Run shape is bimodal:** **14/30 solved in a single loop round**; **9/30 ran to the 25-round cap**. Almost nothing
  in between — the loop either nails it immediately or gets stuck.
- **Wasted time is concentrated:** 1908 s total, median run 19.6 s — but **63 % of all wall-clock (1212 s) was burned
  in the 8 catastrophic runs**. **233** failed `python_run` calls in total, all the empty-arg error.

**Run taxonomy**

| class | count | what happened |
|---|---|---|
| Clean (0 tool errors) | **16 (53 %)** | 1–3 rounds, 7–20 s — the happy path |
| Recovered (1–19 errors) | **6 (20 %)** | hit the empty-arg bug, escaped after a few rounds |
| Catastrophic (≥20 errors) | **8 (27 %)** | never escaped → 25-round cap, 140–181 s, garbage answer |

**The trigger is question-dependent** (n=2 per question — suggestive, not conclusive):

| question | catastrophic | avg tool errs | avg rounds |
|---|---|---|---|
| `count-vowels` | 2/2 | 25.0 | 25.0 |
| `roman-to-int` | 2/2 | 24.0 | 25.0 |
| `max-subarray` | 1/2 | 13.5 | 18.0 |
| `fib-30` / `reverse-words` / `word-count` | 1/2 | 12.5 | 13.0 |
| `two-sum` | 0/2 | 8.0 | 10.0 |
| `sum-primes-below-100` | 0/2 | 6.5 | 7.5 |
| `gcd` | 0/2 | 1.5 | 3.5 |
| **always clean** (`fizzbuzz-count`, `anagram-groups`, `palindrome-count`, `digit-sum-2-pow-100`, `binary-to-decimal`, `factorial-trailing-zeros`) | 0 | 0.0 | 1–3 |

So the model's tendency to drop the `script` arg tracks **task phrasing/content**, not chance — some prompts reliably
induce it, others never do.

**Two ways the loop reaches its cap**
1. **`python_run` empty-arg cascade** — 8 runs: the identical failing call replayed 25×.
2. **Indecision loop** — `collatz-27` hit ~25 rounds with **~0 tool errors**: it re-planned/re-verified a correct
   answer without ever deciding it was done. Same wasted budget, no error to show for it.

**Confirmed across all 30 runs**
- **`turn_ended.iterations` cumulative-counter bug:** the reported value equals the running cumulative of `run_plan`
  rounds for **every** run (final 296). Definitive.
- **Garbage answer + false `outcome:ok` on cap exhaustion:** 8/8 catastrophic runs.
- **"already exists" answer mis-framing:** 8/30 runs.

---

## Resolution — fixes implemented & verified (branch `fix/agent-loop-robustness`)

All four defects are fixed and verified end-to-end by re-running the harness on a fresh server (10 runs,
sessions `s_167`–`s_176`, including the previously-2/2-catastrophic `count-vowels` and `roman-to-int`):

| metric | before (30 runs) | after (10 runs) |
|---|---|---|
| catastrophic runs (≥20 tool errors → cap) | 8 (27 %) | **0** |
| total failed `python_run` calls | 233 | **0** |
| garbage `observed turn.iteration` answers | 8 | **0** |
| `turn_ended.iterations` | cumulative → 296 | **per-turn** (1,5,1,1,1,1,1,11,13,5) |

- **Fix A** (`engine.rs`) — snapshot the `turn.iteration` evidence count at turn start and subtract, so
  `iterations` is per-turn, not cumulative across a persistent server.
- **Fix 1** (`toolchains.rs` + `analyze.rs`) — a zero-arg `python_run` now runs the most-recently-modified
  `.py` (DWIM). Verified directly: s_174/175/176 emit the empty-input `python_run` (the exact old bug) and
  every call now **succeeds**. Plus a general analyzer check turns a required-arg op called with *no* args
  into a re-plannable compile error.
- **Fix 2** (`loop_host.rs`) — retry-breaker: a stalled loop (identical `run_plan` transcript repeating)
  escalates the fed-back directive at 2× and ends the turn honestly at 4×, instead of running to the 25-cap.
- **Fix B** (`runtime.rs`) — an explicit `return <expr>` now yields the returned value (not a stale `last`),
  so genuine cap-exhaustion surfaces the honest `max_iter` message + outcome rather than leaked machinery.

Tests: `cargo test` green across the touched crates (fmt + clippy clean), with a new regression test per fix
(`required_op_with_no_args_is_rejected`, `explicit_empty_return_wins_over_stale_last`,
`retry_breaker_escalates_then_arms_a_hard_stop`, `newest_py_resolves_the_sole_script_else_none`).

**Residual (not catastrophic; future work):** the "indecision" pattern (a couple of runs did 11–13 rounds with
no errors) and the occasional "already exists" framing remain — a `verify-then-stop` heuristic would address them.

---

## Categorized problems & fixes

### Tool usage & errors
- **`python_run` invoked with no `script`/`module` argument** — _≥1 failure in 14/30 runs; catastrophic in 8/30. The single highest-impact bug._
  Root cause confirmed from the emitted plan ASTs (`flow.db` `values_store`, s_156): for the same task the model
  alternates between the working `{"op":"python_run","args":[{"kind":"lit","value":"solution.py"}]}` and the failing
  `{"op":"python_run","args":[]}` — it intermittently calls `python_run` with **no path**, expecting it to run the file
  it just wrote. The op schema (`crates/flux-tools/src/toolchains.rs:118-125`) marks **none** of `script`/`module`
  `required`, so a zero-arg call is schema-valid, compiles clean, and only fails at *runtime* (`toolchains.rs:144-155`).
  **Fix (implementing):** (a) make zero-arg `python_run` DWIM — default to the most-recently-written `.py` in cwd; and
  (b) add general required-param validation in the analyzer so genuinely-missing required args become re-plannable
  compile errors for all ops.

### Solution correctness
- ✅ **30/30 correct on disk; 0 incorrect — but agent-confirmed only 22/30.** The 8 catastrophic runs passed only
  because `write` left a valid `solution.py` the harness ran; the agent never verified its own output and exited with
  a garbage answer. On-disk correctness ≠ agent-confirmed correctness.

### Loop efficiency (iterations / wasted work)
- **No adaptation to a repeated identical tool error → runs to the iteration cap** — _8/30 runs hit the cap this way_.
  The model replays a byte-identical failing round 25× (`op_histogram` = every op at 25; 140–181 s). This amplifier,
  not the empty-arg mistake itself, makes the stuck runs *long*.
  **Fix (implementing):** a retry-breaker — detect the same `(op,input_hash)` failing N× in a row; escalate feedback,
  then hard-stop the turn honestly.
- **Indecision loops also exhaust the cap (no errors)** — _seen 1× (collatz-27)_: ~25 rounds re-verifying an
  already-correct answer. Mitigated by the honest cap-exit fix below; a full verify-then-stop is future work.
- **`turn_ended.iterations` is cumulative across turns** — _30/30, confirmed_. `engine.rs:238` counts
  `turn.iteration` observations over the persistent executor; never reset per turn (5 → … → 296).
  **Fix (implementing):** snapshot the count at turn start and subtract, so the value is per-turn.

### Output & communication quality
- **Loop-exhaustion yields a garbage answer and a falsely-`ok` outcome** — _8/8 catastrophic runs_. The reply is the
  literal `observed \`turn.iteration\`` with `outcome:ok`. `engine.rs:219-235` already has honest `max_iter` handling
  that never fires, because `runtime.rs:429-439` returns a stale value instead of the empty `$answer`.
  **Fix (implementing):** make an explicit `return <expr>` yield the returned value, so cap-exhaustion surfaces the
  honest `max_iter` message + outcome.
- **Final answer mis-reports created work as pre-existing** — _8/30 runs_ ("already exists" though created this turn).
  Lower priority; deferred.

### Environment & setup
- **A2A sessions share the persistent `~/.flux/events.db` with no per-task isolation** — the cumulative-counter bug is
  a symptom of per-server state not reset per task (matches the unpruned-sessions TODO in `a2a.rs`).

---

## Run log (first 7 of 30; remainder summarized in the aggregate)

### Run 1 — `two-sum` — `s_133` — ✅ pass — 34 s
- `python_run` failed 4× (empty-arg); iterations 2–4 replayed the identical write+call before iter 5 varied and succeeded.

### Run 2 — `fizzbuzz-count` — `s_134` — ✅ pass — 21 s
- 0 tool errors, but re-ran/re-wrote an unchanged file; "already exists" mis-framing.

### Run 3 — `sum-primes-below-100` — `s_135` — ✅ pass — 9 s
- Clean & efficient (1 write + 1 run). First confirmation of the cumulative iteration-counter bug.

### Run 4 — `fib-30` — `s_136` — ✅ pass — 147 s ⚠️
- 1st catastrophic run: 25 empty-arg failures to the cap; garbage `observed \`turn.iteration\`` answer, `outcome:ok`.

### Run 5 — `anagram-groups` — `s_137` — ✅ pass — 18 s
- Clean & efficient; "already created" mis-framing.

### Run 6 — `reverse-words` — `s_138` — ✅ pass — 8 s
- Clean & efficient (fastest); answer framed correctly.

### Run 7 — `count-vowels` — `s_139` — ✅ pass — 140 s ⚠️
- 2nd catastrophic run, identical mode to s_136.

### Runs 8–30 — `s_140`–`s_162` — all ✅ pass on disk
- See aggregate: 6 more catastrophic runs (incl. both `roman-to-int` and the 2nd `count-vowels`); every catastrophic
  run reproduced the garbage-answer + false-`ok` pattern; cumulative counter held to 296.
