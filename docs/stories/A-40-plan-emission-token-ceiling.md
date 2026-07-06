---
id: A-40
title: Oversized plan emission dies at max_tokens — split, don't retry the whole plan
pillar: Agent
status: done
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: "I-03's tbench regression signature: execute-phase plans on write-heavy tasks truncate at the 16384 emission ceiling and the loop re-pays whole-plan retries (31 steps / $0.76 per fibonacci-server trial, 4× baseline) without ever completing"
---

# Oversized plan emission dies at max_tokens — split, don't retry the whole plan

## Goal
I-03's terminal-bench post leg failed both tasks with the same mechanical signature: `planner
output was truncated at max_tokens (16384) before it finished the plan — raise --max-tokens or
split the request into smaller steps`, retried repeatedly at full price. A plan that cannot fit
the emission ceiling should get *smaller* on retry (fewer steps now + a continuation turn, or
payload-bearing steps split out), not be re-emitted whole until the budget or the step cap dies.
The design doc's "I-03 measurement results" section carries the measured evidence.

## Acceptance
- [x] Truncated-at-max_tokens plan emission is detected as its own repair class (it already
      surfaces as a distinct runtime error) and the repair prompt instructs a *split*: emit the
      plan's first N statements + explicit continuation intent, or hoist large literal payloads
      (file writes) into their own follow-up plan — failing-first test on the repair path.
- [x] A second truncation on the already-split plan does not loop: bounded retries with the
      existing stall/budget guards, then a legible failure naming the ceiling.
- [x] `"""` multi-line strings (L-39) are used by the emission prompt's guidance for large write
      payloads so the JSON-escaping bloat stops inflating token counts (planner grammar already
      teaches the spelling — verify the repair guidance references it).
- [x] The I-03 fibonacci-server scenario (write server.js + start + verify, 16384 cap) completes
      on the phased loop in a harness re-run or an equivalent eval fixture; measured
      before/after cost of the failure mode recorded here.
      Both halves done: the eval-shaped fixture
      (`compile_turn_completes_fibonacci_scenario_via_truncation_split_repair`) passes, AND the
      live harness re-run (fibonacci-server × 3 trials, same model/dataset/ceiling, fixed binary)
      shows the failure mode **eliminated** — see the 2026-07-06 live re-run Progress entry.
      Task *checks* remain 0% for an independent, newly-diagnosed harness gap (shell disabled in
      the tb container — filed I-04), which pre-existed A-40 and hits every leg equally.
- [x] Gate green (package-scoped: `cargo build/test/clippy/fmt -p flux-flow`; full-workspace gate
      not re-run as part of this story).

## Progress
- **2026-07-06**: Implemented the A-40 truncation split-repair in
  `crates/flux-flow/src/compile.rs`, `compile_turn_inner` (the `Some(StopReason::MaxTokens)` branch,
  originally ~line 658).
  - New `const TRUNCATION_REPAIRS: u32 = 2` (just above `compile_turn_inner`): bounds how many
    split-repair attempts a truncation gets before the turn fails legibly. A local
    `truncation_repairs: u32` counter tracks spend per turn.
  - New free functions (placed after `hidden_ops_rejection`, new `-- A-40 --` section):
    - `truncation_repair_text(arm: EmissionArm, max_tokens: u32) -> String` — builds the arm-aware
      repair guidance (both arms: name the ceiling, "Do NOT re-emit the same plan", emit a SMALLER
      plan with only the first few statements, OMIT `complete`, one file write per plan; text arm
      additionally references the `"""` verbatim multi-line spelling the grammar already teaches at
      `build_text_grammar` — L-39 — the JSON arm does not get that hint).
    - `push_truncation_repair(messages: &mut Vec<Message>, assistant_pushed: bool, repair: String)` —
      the message-shape-discipline helper: pushes a fresh `Message::user_text` when this step's
      assistant message was already pushed (non-empty preamble, mirrors the `hidden_ops_rejection`
      precedent), otherwise appends the repair onto the tail of the last user message already in
      `messages` (empty-preamble case — avoids a user-after-user sequence).
  - The MaxTokens branch now: if `truncation_repairs < TRUNCATION_REPAIRS`, bump the counter, build
    the repair text, feed it via `push_truncation_repair`, `trace_step`, `continue`; once the budget
    is spent, `return Err` with a message naming the ceiling and that the split repair was attempted
    (contains "truncated", "max_tokens", the ceiling value, and "split-repair attempt(s)").
    `usage.accumulate` needed no change — it already runs before this branch on every call.
  - Updated `docs/designs/multipass-agent-loop.md` with a new "A-40: truncation split-repair"
    subsection right after the "I-03 measurement results" section, describing the mechanism.
  - Tests added/updated in `crates/flux-flow/src/compile.rs` (`mod tests`), all passing:
    - `compile_turn_errors_on_max_tokens_truncation` (updated) — always-truncating mock now supplies
      `1 + TRUNCATION_REPAIRS` truncated responses; asserts the bounded error names "truncated",
      "max_tokens", "16384", and "split-repair attempt".
    - `compile_turn_bounds_truncation_repairs_then_errors` (new) — a call-counting mock provider
      asserts *exactly* `1 + TRUNCATION_REPAIRS` (3) provider calls before erroring — not step-budget
      (8) exhaustion.
    - `compile_turn_repairs_max_tokens_truncation_with_empty_preamble` (new) — truncation with no
      preamble text, then a valid small plan → `TurnOutput::Plan`; asserts the second request's
      `messages` stays length 1 (repair appended to the existing user message, not a new one) and
      passes a new `assert_valid_alternation` helper (no empty message, no two consecutive same-role
      messages).
    - `compile_turn_repairs_max_tokens_truncation_with_preamble_text` (new) — truncation with
      non-empty preamble, then a valid small plan → `TurnOutput::Plan`; asserts the second request's
      `messages` is `[user, assistant(preamble), user(repair)]` and passes `assert_valid_alternation`.
    - `truncation_repair_text_is_arm_aware` (new) — direct unit test on the helper: the text arm's
      repair contains `"""`, the JSON arm's does not; both name the ceiling and "SMALLER".
    - `compile_turn_completes_fibonacci_scenario_via_truncation_split_repair` (new) — the
      Acceptance-item-4 offline fixture: a write-heavy truncated preamble (mirrors the I-03
      fibonacci-server transcript shape) followed by a small post-split plan with no `complete`;
      asserts the turn returns `TurnOutput::Plan` with `complete: None` (the phased/A-14 loop
      continues next round) instead of erroring.
    - New module-level test helpers: `assert_valid_alternation` and a `RequestCapturingMock`
      provider (records every `Request` seen, for inspecting the repaired message list).
  - Gate (package-scoped, run repeatedly through the change): `cargo build -p flux-flow` clean;
    `cargo test -p flux-flow` — 219 passed, 0 failed (includes 3 doctest/integration binaries: 219 +
    3 + 1, all green); `cargo clippy -p flux-flow --all-targets -- -D warnings` clean; `cargo fmt
    --all` then `cargo fmt --all -- --check` clean — `git status`/`git diff --stat` confirmed fmt
    only touched `crates/flux-flow/src/compile.rs` (no other files reformatted).
  - **Remaining**: Acceptance item 4's live half (terminal-bench harness re-run on
    fibonacci-server/chess-best-move + measured before/after cost) is NOT done — that is the
    orchestrator's follow-up leg. Status is left `in-progress` (not `done`) for that reason.
- 2026-07-06 (later) — **live re-run DONE, failure mode eliminated; story closed.**
  fibonacci-server × 3 trials, same model (`openrouter-anthropic/anthropic/claude-sonnet-4.6`),
  same dataset/ceiling (16384), freshly built musl binary with the fix, post-leg only (the
  "before" is I-03's own post leg). Flow + report:
  `bench/tbench-compare/results/a40-fix/` (`eval-a40.flux`, `run.log`).
  - **Before (I-03 post leg)**: every trial died in whole-plan truncation retries —
    `planner output was truncated at max_tokens (16384)` — 31 steps, 22.9k out tokens,
    **$0.7553/trial**, 0% checks.
  - **After**: `truncated at max_tokens` occurs **0 times across all 3 trials** (the split repair
    is silent by design — a truncation now costs one in-loop repair, not a turn death — so signature
    absence IS the fix working and/or plans now fitting). Sampled trial transcript: the write-heavy
    plan completes first-shot — **4 steps, 73.3s, out 5.9k, $0.3482** (less than half the
    before-cost, and the before never completed at all).
  - Honest caveat: task *checks* stayed 0% for an independent, pre-existing harness gap discovered
    while validating — the tb custom agent (`crates/flux-eval/terminal_bench/flux_agent.py`) runs
    `flux run --yes` in the container forwarding only provider keys, so the `shell` group is never
    enabled and the agent cannot START the server it correctly wrote (its final message says
    exactly that). This hits every historical leg equally (I-03 baseline included) → filed
    **I-04** (enable shell in the tb container harness + full re-run) rather than folding it here.

## Notes
- Evidence: `bench/tbench-compare/results/i03-go/post-report.txt` (fibonacci-server 31 steps /
  22.9k out tokens / $0.7553 per trial, all checks failed; chess sub-agent turn lost to the same
  truncation). Baseline completed equivalent plans under the same 16384 ceiling — the phased
  loop's execute-phase plans are bigger.
- Related: A-30/A-31 emission-repair machinery (repair rides cached segments), L-39 multi-line
  strings, C-35 (the gather-round cache economics of retries).
