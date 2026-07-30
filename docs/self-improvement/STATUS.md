# Self-improvement: status & journey

_Last updated: 2026-07-14 (targeted adaptive-budget and latency hardening). Substantive autonomous-loop state is
frozen at round 3, 2026-07-06 — the initiative remains ON HOLD._

This is the honest, dated record of where the self-improvement loop stands and how it got here —
including the bugs each live run surfaced, the first kept gain, and the caveats that keep the claims
defensible. For how the loop works, see [DESIGN.md](DESIGN.md).

## TL;DR

- **ON HOLD / de-prioritized since 2026-07-06 (user priority call).** The machinery is proven
  end-to-end, but active development is paused (focus shifted to hardening/docs/cleanup) and the
  headline gain — a statistically clean, grader-confirmed improvement at **trials ≥ 3** — is **not
  yet achieved**. Resume at I-05's two queued chain fixes, then fund round 4. See the
  [2026-07-06 journey entry](#journey-the-runs-and-what-each-one-taught-us) and stories I-01 / I-05.

- **2026-07-10 audit hardening:** the improve-tbench flow now fails closed when no implementation is
  produced, accepts fenced/prose-wrapped candidate arrays, and the eval keep-gate rejects malformed
  or invalid telemetry instead of treating it as a cheap zero. Trial concurrency is bounded and
  deterministic, and terminal-bench usage remains unavailable rather than falsely zero when absent.

- **2026-07-13 targeted live harness hardening (outside the paused autonomous loop):** tutorial E2E
  latency analysis found 636 registered operations and a 27.5k-token unrelated-plugin tax on every
  planner call. A-67 now puts otherwise-ungrouped plugin ops behind audited turn-intent groups. With
  the same binary, HOME, workspace, OpenRouter model, and prompt, planner input fell 41,567 → ~14,100
  tokens and reported cost $0.0106 → $0.0025; naming Slack surfaced only `plugin.slack` (15.3k).
  Correctness caveat from the same experiment: Gemini 2.5 Flash was fast (5.7s) but falsely claimed a
  file write without emitting a plan, while GPT-5-mini was correct but spent 46.2s planning. Model
  latency is not a substitute for the structural catalog fix. The autonomous improvement initiative
  remains on hold; this was a user-scoped, live before/after hardening task.

- **2026-07-13 follow-on trace + startup hardening (also outside the paused loop):** a credential-free
  native model trace exposed another 5.1k input tokens from false-positive skill activation and about
  2.5s outside the provider. Skills are now manual-only (`--skill` / explicit `AgentSpec.skills`),
  reducing the same-workspace probe 28,449 → 23,283 input tokens. The no-plugin-HOME control then
  isolated installed-plugin startup: the first `buffer_unordered` fix was illusory because plugin
  futures did synchronous verification/spawn work before yielding. Bounded Tokio tasks reduced three
  warm 18-plugin mock runs from 2.222–2.246s to 0.585–0.592s, and the live non-provider gap to ~0.75s.
  A same-task Codex low/high effort pair proved the newly wired AgentSpec effort reaches the wire;
  both trivial answers were correct with zero reasoning tokens, so it is propagation evidence, not
  an effort-quality/default claim. The autonomous initiative remains on hold.

- **2026-07-13 install-gate flake found by the operator:** `task install` failed 13 `flux-system`
  process tests as one cluster. Sandbox discovery tests temporarily replaced process-wide `PATH`;
  their mutex serialized only other mutators, so unrelated parallel tests intermittently lost
  `sh`, `printf`, `env`, and `sleep`. Discovery tests now inject PATH into pure helpers and never
  mutate the process value; temporary workspace creation reads TMPDIR under the existing guard.
  Verification: 20 consecutive 64-thread `flux-system` runs passed, followed by the exact
  `task install` (workspace library tests plus replacement of both `flux` and `flux-lsp`).

- **2026-07-13 matched-effort request-shape/citation probe:** low, medium, and high Codex runs all
  computed the support fixture correctly, but low/medium invented source filenames because gathered
  values reached the next model call as anonymous `[read]` blocks. The runtime now retains bounded,
  allow-listed read/grep provenance in feedback; two fresh runs cited every real source correctly.
  The trace also isolated a latency defect: four consecutive matched runs emitted the same invalid
  positional `grep` arguments, and the repair call alone cost 6–10 seconds plus roughly 17k input
  tokens each time. `Call.args` had no schema description; its named-object rule lived only in remote
  prose, and the error illustrated the wrong abstraction level. Putting the contract on the field
  and emitting the exact AST repair shape changed matched first-plan validity from 0/4 to 3/3. Two
  low-effort runs finished correctly in 13.2/14.4s; a medium run used the saved call for useful
  retrieval and took 26.6s, so broader latency remains open. This remains targeted harness hardening
  outside the paused autonomous loop.

- **2026-07-13 fast-model/schema probe:** the exact emitted request corrected an important working
  assumption: the planner receives the real schema only for `emit_plan`; a call inside its AST is
  still generic `op: string` + `args: Node[]`. Operation-specific schemas stay in the registry for
  validation and are summarized only as catalog prose (`read(...)`, `ai.reason(...)`) until an error
  renders the live signature. On the same adversarial support fixture, OpenRouter Gemini 3.5 Flash
  was correct in 2/3 merged-schema trials (7.1–22.8s) and fabricated every central fact once after a
  failed plan; its strict-schema control still took 20.6s and invented the plan name. OpenRouter
  DeepSeek V4 Flash Nitro was correct in 3/3 but took 25.6/30.7/53.3s and 4–10 model calls: it emitted
  malformed AST once and attempted four direct `read` calls that Flux safely rejected. Codex low's
  matched post-fix runs remained 3/3 at 13.2–14.4s with two calls. The next structural experiment is
  phase/intent-narrowed **real operation schemas** (or deterministic lowering of native tool calls
  into Flux plans), not more global prompt prose; keep merged-vs-strict/text as measured controls.

- **2026-07-13 adaptive-loop cutover (A-73, outside the paused autonomous loop):** the A-71/A-72
  experiment is now the default authored Flux-Lang outer loop rather than a hybrid compiler mode.
  Models detect intent and explore with exact native operation schemas; the host freezes effects into
  an `ActionBatch`, obtains a batch-bound one-shot approval receipt, and executes through the guarded
  runtime. Typed decisions use the existing durable `await`, CLI/TUI show routing and exploration,
  and callers can explicitly select a different authored loop or register independently typed model
  and SDK stages. The NL-to-Flux compiler, planning commands, corpus/export surfaces, and implicit
  `.flux/agent-loop.flux` override are removed. The installed-binary gate passed 12/12: Codex gpt-5.5
  (`s_1150`–`s_1152`) used 4/4 provider calls, Gemini 3.5 Flash (`s_1153`–`s_1155`) 4/4, DeepSeek V4
  Flash Nitro (`s_1156`–`s_1158`) 4–5/7, and GPT-5-mini (`s_1159`–`s_1161`) 4/6. Every answer cited
  all required real paths, fabricated no path, and made zero legacy planner calls. Deterministic root
  inventory guidance closed the only initial path-discovery miss without relaxing the gate. The
  reproducible test is `scripts/eval-adaptive-support.sh`; `task install`, workspace
  build/test/clippy/fmt/codegate, generated-doc sync, SDK/CLI/A2A, and hermetic voice tests are green.

- **2026-07-13 adaptive-loop hardening (A-76, outside the paused autonomous loop):** deterministic
  tests exposed and closed three post-cutover structural gaps: repeated decisions now reuse one
  durable await, an approved non-idempotent action executes exactly once even when a later question
  suspends the turn, and capability stickiness is isolated by session on a shared engine. Loaded
  integrations contribute compact alias/capability/URL routing evidence, with ambiguity resolved
  before schemas load and unwired plugins excluded. A durable 12-call logical-run ceiling survives
  decision resume, stage-level model/effort/token/call policy is explicit, and each built-in request
  now records correlated TTFT/duration/usage/schema measurements. An installed DeepSeek V4 Flash
  Nitro smoke routed a live-time request through `now` and exposed its 7.5-second stage breakdown;
  `task install` and the complete root/plugin gates pass. The live support matrix still requires a
  fresh three-trial run before making a new cross-model quality claim; its report now includes
  per-stage call counts and latency.

- **2026-07-14 adaptive-budget coherence (A-77, outside the paused autonomous loop):** the public
  logical provider-call ceiling no longer collides with a hidden 12-round native clamp. Normal turns
  now default to 50 model calls, authored `ai_segment.max_rounds` is honored exactly, and the
  separate Flux decision/batch loop defaults to 50 iterations. CLI, project/user config, AgentSpec,
  and SDK controls remain distinct; 50/51-boundary and early-invalid-value tests pin the behavior.
  Capability visibility, authorization, approval, dispatch, and guarded IO are unchanged. The full
  workspace gate and exact `task install` passed, including all 110 `flux-system` tests.

- **2026-07-14 paired intent-latency evaluation (A-78, outside the paused autonomous loop):** a
  redacted CLI evaluator ran 36 screening turns and 120 alternating-order confirmation turns across
  Codex gpt-5.5, GPT-5-mini, DeepSeek V4 Flash Nitro, and Gemini 3.5 Flash. A 512-token intent cap
  was rejected: per-model intent medians changed by +1.7%, -9.5%, -15.3%, and -6.8%, none meeting
  the 20% keep threshold; GPT-5-mini fell from 15/15 to 11/15 and several end-to-end medians
  regressed. Shipped intent defaults remain unchanged. Slack was selected by every model and no
  write executed, but Gemini failed before approval because its endpoint rejected surfaced array
  schemas without `items`; A-81 tracks this separate provider-portability defect. The autonomous
  improvement initiative remains on hold.

- **2026-07-13 post-cutover semantic-expansion hardening (A-74, outside the paused autonomous
  loop):** live session `s_1162` selected Slack correctly, then needed a second family to retrieve a
  current Bitcoin price. The next native round rejected the accumulated capability state because it
  intersected the valid later signal with the immutable turn-start surface. Turn-local semantic
  visibility now grows monotonically in the durable adaptive state while every native round still
  re-applies the live registry, agent tool, bare-deny, `with_tools`, and authored-stage ceiling. A failing-first
  Slack→web-search fixture pins the exact regression; denied and operator-gated operations remain
  unavailable, and genuine drift names each missing operation and reason. The unrelated code-review
  edits were ruled out by process/source timestamps: the failing executable predated them.

- **2026-07-13 post-cutover routing completeness (A-75, outside the paused autonomous loop):** live
  session `s_1169` selected no capability for `get the current time` even though `now` was registered.
  Virtual-family previews had silently omitted every member after eight, and the router did not state
  that live facts require evidence. A failing-first arbitrary twelve-operation fixture now pins a
  lossless routing index for all ungrouped operations; gather safety no longer conflates a fresh,
  non-cacheable result with a mutating action. Candidate session `s_1170` routed `core` → `now` and
  returned its actual UTC value without approval (7.1s, 4.7k context).

- **The loop works end-to-end.** Every stage fires on real Docker / terminal-bench: baseline eval →
  reviewer → aggregate → planner → `git_snapshot` → worker → `guard_protected` → `gate_check` →
  candidate eval → `score_compare` → keep+tag **or** revert → `improve_log`.
- **It has improved the harness for real, once.** On a `fibonacci-server` run, the loop autonomously
  diagnosed a real failure mode, fixed flux's shipped system prompt, measured the candidate beating the
  baseline on partial credit, and **kept + committed + tagged** the change. Details + caveats below.
- **It is auditable.** Every decision lands in `improve-log.jsonl`, the `events.db` RunEvent trace,
  git tags, and asciinema casts. The agent never grades itself.
- **What's not yet done:** a statistically clean (trials ≥ 3) headline gain — which must come from
  **terminal-bench** (the synthetic suite calibrated out as saturated) — in-container metrics for
  terminal-bench, and breadth. See [Known gaps](#known-gaps).

## What's proven

| Claim | Status | Evidence |
|---|---|---|
| Machinery runs end-to-end | ✅ proven | multiple live tb runs; every op fires |
| Correct **revert** on a non-improvement | ✅ proven | revert run; grader caught a real flux bug (below) |
| Integrity guard restores tampered grader | ✅ proven (fired live) | `guard_protected` rolled back a worker edit to `crates/flux-eval` |
| Correct **keep + commit + tag** on a real gain | ✅ proven (once) | commit `3c86874` (the disposable tag/branch have since been pruned; the fix now lives on `main` as `f0ede92`) |
| Statistically clean headline gain (trials ≥ 3) | ⛔ not yet | next chapter |

## The epic's arc (milestones)

What's been built, so a continuer knows the terrain. All landed on `main` unless noted.

- **M1 — crate + offline slice.** `flux-eval` scaffold (spec / adapter / runner / metrics / score),
  the mock adapter, and `flux-cli --output json` + `flow run <file>`.
- **M2 — mining substrate.** flux-flow Usage capture + deterministic pain-point mining
  (`painpoints_collect`). _Token/cost capture is only partly done — see [Known gaps](#known-gaps), #12._
- **M3 — review → aggregate → derive.** Authored the review→aggregate→derive flow + a fixture test
  that validates the checked-in flows against the live op set.
- **M4 — keep/commit loop.** The loop ops (`git_*`, `gate_check`, `score_compare`) + the runner script
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
- **Phase D — roles + docs.** Tracked sub-agent roles + design docs. (The "suite breadth" of this
  phase was the toy local suites, since removed — see below.)
- **Phase E — live validation.** The runs in the journey below; bugs found + fixed.
- **Partial credit + trials=2 + the kept-gain proof run** — the most recent work (below).
- **Removed the toy local-suite path** — deleted `suites/`, `examples/improve.flux`, and
  `scripts/improve.sh`. Terminal-bench is now the single real eval; the `mock` adapter remains only as
  the offline smoke fixture (`examples/eval-smoke.flux`). `PROTECTED` was corrected to guard the real
  loop (`bench/`, `examples/improve-tbench.flux`) instead of the deleted toy paths.

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

**Where it lives:** the loop produced this on a disposable worktree branch
(`improve-tbench/20260626-…`) as commit `3c86874` ("improve: adopt candidate (terminal-bench gain)",
`crates/flux-agent/src/lib.rs` +43/-1) with tag `improve-tbench-0-3c8687…`. Those throwaway refs have
since been **pruned** — the kept fix was brought to `main` verbatim as commit `f0ede92` (see
[Suggested next steps](#suggested-next-steps) #1), which is where it lives now.

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
5. **2026-07-02 — synthetic calibration + tbench-over-OpenRouter smoke (no loop run).** The I-01
   staged calibration ran `flux eval synthetic --trials 5` twice: **the synthetic suite is stable but
   saturated** — Sonnet 4.6 *and* Haiku 4.5 (via OpenRouter) both score 1000/1000 with mean_iters 1.0.
   Zero headroom: it is a **regression floor, not a gain vehicle**; the headline gain must come from
   terminal-bench. The tbench plumbing was then smoke-proven end-to-end over OpenRouter (key forwards
   into the container, model plumbs through `eval_run` → `tb --model`, musl binary builds): 1 task ×
   1 trial ran for real. Result 0/1 (`parse_error`) — and instructive: the agent **functionally solved**
   `fibonacci-server` (server up, every curl check correct) yet burned the 30-plan-iteration cap stuck
   on one step (480s, $0.58, never credited). That failure shape motivated the
   **multi-pass agent loop epic** (`docs/designs/multipass-agent-loop.md`). The full improve-loop run
   was postponed (user call, 2026-07-02). The former model-facing `flux_binary` workaround has since
   been removed; terminal-bench now takes the evaluated binary only from trusted host
   `FLUX_EVAL_BINARY`.

6. **2026-07-06 — two eval-infrastructure defects found and fixed; the funded I-01 round launched.**
   (a) **I-04 — the shell group was OFF inside every tbench container to date**: `flux_agent.py`
   forwarded only provider keys, so in-container flux never surfaced `bash` — on the A-40 validation
   run the agent wrote a correct `server.py` and then honestly said it couldn't start it. Fixed
   (`FLUX_ENABLE_BASH=1` + a pinning test in flux-eval); single-trial verify: fibonacci checks
   **0% → 83%**. All pre-2026-07-06 containerized numbers are equally depressed (run 5's
   "functionally solved" shows other ops occasionally compensated, but the handicap was
   structural); decision: corrected harness carries forward, **no I-03 re-baseline**, result dirs
   are era-labeled. (b) **The improve loop was unrunnable on `main`**: `improve_log` grew a
   required `record` param and the checked-in flows went stale — invisible to CI because
   `flows_validate` ran only `analyze_flow` while `flux flow run` gates on `lower`
   (required-param/type walk). The fixture test now runs the same `lower` gate (reproduced the
   exact runtime rejection first), and both improve flows are fixed. (c) With both fixed, the
   **funded I-01 round is running** on branch `improve-tbench/20260706-130553` (HEAD = corrected
   harness, in-container evals routed via OpenRouter by an operator commit on the disposable
   branch — provider routing, not a graded change). (d) The first full round (attempt 3) completed
   end-to-end but was a **null round**: the flows predate the explicit `obj`/`list` value-template
   nodes and carried node-maps *inside* `lit` args — the runtime no longer implicitly resolves
   those, so `task` crashed (fixed first), then `improvements_aggregate`/`change_implement`/
   `score_compare` silently operated on unresolved maps: the reviewer's six REAL findings (its top
   one: failed-`bash` output is truncated to `last: [exit 1] (-v for full)`, hiding the actual
   error body from the model — prime next-round fodder) collapsed to "0 improvement candidate(s)"
   styled as success, the worker had nothing to do, and score_compare tied 0-vs-0 → correct revert
   of a no-op. All call sites converted to obj templates (both flows, main + loop branch), and
   `improvements_aggregate` now NAMES swallowed non-empty input in its view instead of silent zero
   (failing-first test). Corrected-era baselines for the record: checks 28% (attempt 2) / 42%
   (attempt 3) vs the shell-off era's 14% — and within attempt 3, fibonacci hit 83% checks with
   the agent starting its server via `bash`. (e) **Attempt 4 (the fully-fixed flow): the complete
   machinery ran end-to-end with real payloads for the first time, and produced a correct
   revert.** Reviewer → 6 candidates → planner → 2 concrete tasks → worker IMPLEMENTED both
   (write-tool full-content echo view; a verify-before-declaring-done bullet in
   DEFAULT_SYSTEM_PROMPT) → guard intact → gate green (29.4s) → candidate eval → **278 vs 278**
   → strict revert, `reason: no_improvement`, both tasks preserved verbatim in
   `improve-log.jsonl`. Two lessons for the next round: (1) cross-round baseline noise is real —
   28% vs 42% between attempts, driven by chess-best-move flakiness (vision-dependent, plus tb
   registry HTTP 429s polluting one grading leg) — within-round comparison stays valid but a
   stabler task set (or higher trials) is needed for a defensible headline; (2) the reviewer's
   severity-5 finding — failed-`bash` output truncated to `last: [exit 1] (-v for full)`, hiding
   the error body from the model — was NOT among the planner's two picks and remains the top
   unplayed candidate (4 more queued in the round record). **I-01 stays open**: the machinery is
   now proven; the statistically clean gain is not yet achieved. (f) **Round 3** (the I-05
   sharpened setup: fibonacci-server×5 scored substrate, `FLUX_IMPROVE_EVAL_MODEL` knob,
   weight-ranked planner prompt — all worked) surfaced chain defect #3: the **planner answered in
   prose** instead of the bare JSON array → `change_implement` extracted 0 tasks → the candidate
   leg measured an unchanged tree → tie → correct revert. Two fixes queued in I-05 (skip the
   candidate leg on `implemented == 0`; planner-output hardening + loud `change_implement`).
   **The initiative is ON HOLD as of 2026-07-06 (user priority call)** — resume at I-05's queued
   fixes, then fund round 4 on the fully-hardened chain. Every chain seam has now either worked
   live or carries a named guard; the loop found and permanently fixed four eval-infra defects
   (I-04 shell, two value-template drifts, the flows_validate lower-gate) in one funded day.

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

- **Stable-baseline synthetic loop + partial-credit-aware scalar + durable token capture** (I-01,
  offline half). `SuiteScore::scalar()` now tracks `mean_check_pass_rate` (a sub-check-only gain tags
  `…-833`, not `…-0`); per-turn `Usage` is persisted on the event store's `TurnEnded` and summed back
  into `RunResult.tokens` (the `mean_tokens` tiebreaker is real for the local/synthetic adapter); and
  `examples/improve-synthetic.flux` + `bench/run-synthetic-loop.sh` add a no-Docker, `trials = 5`
  loop on the 16-riddle synthetic suite — the vehicle for the still-open clean headline gain.
- **Partial-credit scoring** — the terminal-bench adapter parses `parser_results` into
  `mean_check_pass_rate`, used as the first tiebreaker after full-pass-rate, so the loop sees 5/6 → 6/6
  progress instead of only binary pass/fail (`2199477`).
- **trials ≥ 3** (raised from 2), plus a **per-decision audit log** appended to
  `.flux/eval/improve-log.jsonl` each round (`81fe021`). `improve-tbench.flux` was later sharpened to
  **trials = 5** on the single stable fibonacci-server task (I-05); `improve-multi.flux` stays at
  **trials = 3**.
- **Tracked sub-agent roles** in `crates/flux-eval/agents/`, seeded into the worktree by the runner (`4930bf9`).
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

- **Partial-credit-aware tag scalar.** ✅ Resolved — `SuiteScore::scalar()` now returns
  `round(mean_check_pass_rate*1000)`, so a partial-credit-only gain tags as `improve-tbench-833`
  rather than `-0`. Faithful for binary adapters (where `mean_check_pass_rate == pass_rate`).
- **A statistically clean headline gain.** Still open — and it must come from **terminal-bench**:
  the 2026-07-02 calibration (journey entry 5) found the synthetic suite stable but **saturated**
  (two models at 1000/1000, zero headroom), so `bench/run-synthetic-loop.sh` serves as a
  **regression floor**, not a gain vehicle.
- **Token/cost capture.** ✅ Resolved for the local/synthetic adapter. flux-flow now surfaces per-turn
  `Usage` through the loop (commit `b9a0b06`); the events store persists it on `TurnEnded`, and the
  eval runner sums it back via `load_usage` into `RunResult.tokens`, so `mean_tokens` is a real
  lexicographic tiebreaker. *Still open for terminal-bench:* its flux runs inside the container, so the
  tally must be extracted from the container's `~/.flux/events.db` (the same `TurnEnded.usage` is now
  recorded there). (Task #12.)
- **In-container metrics.** flux's RunEvent trace lives inside the container, so `mean_iterations` /
  `mean_tokens` read 0 for terminal-bench; extract `~/.flux/events.db` from the container for
  deterministic mining.
- **Breadth.** A larger terminal-bench subset and a second real benchmark (SWE-bench Lite behind the
  same `BenchmarkAdapter` trait); a held-out scoring slice to guard against overfitting the chosen
  tasks.

## Suggested next steps

1. ~~**Bring the kept prompt fix to `main`**~~ — ✅ done: `3c86874` (runtime verification +
   background-server + confirm-port) landed on `main` as `f0ede92`, with the regression test
   `default_system_prompt_bash_bullet_has_runtime_checks`.
2. ~~**Fix the tag scalar** to be partial-credit-aware~~ — ✅ done (`score.rs`,
   `round(mean_check_pass_rate*1000)`).
3. ~~**One trials ≥ 5 run on the synthetic suite** for a clean headline gain~~ — **calibrated out
   (2026-07-02, journey entry 5):** the suite is stable but saturated (two models at 1000/1000,
   mean_iters 1.0) — zero headroom; it stays as the regression floor. The headline gain must come
   from **terminal-bench** (plumbing smoke-proven over OpenRouter same day; full loop run postponed
   by user). The runner now supplies its host-owned executable/dataset/rebuild settings through
   `FLUX_EVAL_BINARY` and `FLUX_TERMINAL_BENCH_*`; resume with `bench/run-tbench-loop.sh` at trials ≥ 3
   on tasks with a stable baseline.
4. Optionally, a tracked pre-commit hook to mechanically block un-`fmt`'d commits (enforces the process
   rule above).
