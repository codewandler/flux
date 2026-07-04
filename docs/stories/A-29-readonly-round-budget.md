---
id: A-29
title: "Read-only turns need freshness-INDEPENDENT convergence pressure — a novelty treadmill defeats every redundancy guard"
pillar: Agent
status: done
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: "s_356 (binary WITH A-28): 22 rounds on one question, user-cancelled — nearly every round adds a NEW grep pattern or a window over new lines, so transcript-hash + A-20 keys + A-28 coverage all correctly say 'fresh' and never trip; the loop's only exit is the model choosing prose. Add a count-based read-only-round ladder (nudge → answer-now → honest stop) orthogonal to freshness"
---

# Read-only turns need freshness-independent convergence pressure

## Goal
Stop the third (and general) variation of the read-loop pathology. The execute phase's guards are
all **redundancy** detectors: the transcript-stall hash, the A-20 rename-proof resource keys, the
A-28 per-path line coverage. A model that keeps finding *novel-but-marginal* things to read — a new
grep pattern here, a fresh 40-line window there — is indistinguishable from legitimate research to
every one of them, so a question-shaped turn can burn the full 25-round repeat budget doing real
(but unbounded) investigation and never answer. The missing pressure is **count-based and
freshness-independent**: after N consecutive read-only rounds the loop should escalate ("you have
bound M resources across K rounds — answer now from the session symbols, or name precisely what is
missing"), and after N+k force the honest stop, exactly like the stall ladder but keyed on
*consecutive read-only rounds*, not on redundancy. Legitimate long read-only research stays possible
by raising the ceiling, not by defeating the detector.

## Forensics (s_356 turn 16852, 2026-07-03 19:19–19:29, binary built 19:17 — A-20 AND A-28 present)
- Question: "currently I am using bedrock llm provider. after the turn I see some stats, but I
  never see costs/pricing - why is that". 22 planner rounds (orient + capped gather + ~18 execute),
  user cancelled. Session total: 26 plan_attempted / 337 run events across 3 turns.
- Every round: a `parallel` of 2–3 reads — greps with NEW patterns (`cost|pric`, `rates_for|…`,
  `strip_bedrock_region_prefix`, …) and `read` windows over NEW regions/files (main.rs offsets 290,
  585, 618, 670, 680, 1393, 3505; bedrock.rs; pricing.rs; flux-credentials). Genuinely fresh by
  every existing metric → no guard can (or should) call it redundant.
- Contrast: s_346 = byte-identical re-reads under renamed symbols (fixed, A-20); s_355 =
  window-sliding over one covered file (fixed, A-28); s_356 = the treadmill neither can catch.
  Three sessions, one underlying gap: the loop's only exit is the model *choosing* prose.

## Acceptance
- [x] Failing-first test: a loop emitting N consecutive read-only rounds of genuinely FRESH reads
      (new paths/patterns each round) receives the escalation directive at the documented threshold
      and force-stops at the stop threshold with an honest "answer from what you have / say what's
      missing" message. Today it runs to the repeat budget.
      (`novelty_treadmill_escalates_then_force_stops` — red run confirmed: round 6 carried no
      directive before the fix.)
- [x] The ladder is orthogonal to (and does not weaken) the stall guards: redundancy still
      escalates earlier via A-20/A-28; an effectful dispatch or a prose answer resets the read-only
      counter. (The treadmill test asserts "No NEW evidence" never fires on fresh reads; a
      redundancy stop armed in the same round suppresses the breadth banner; a no-read round
      leaves the counter unchanged so a no-op round can't launder it.)
- [x] Thresholds are named constants with rationale (suggest escalate ≈ 6–8 read-only rounds,
      stop ≈ 10–12; the phased-loop gather cap already grants 3 pre-execute rounds), overridable
      via config for legitimately read-heavy workflows ([limits] section), pinned by test.
      (`READONLY_ROUNDS_ESCALATE = 6` / `READONLY_ROUNDS_STOP = 10`;
      `[limits] readonly_rounds_escalate/_stop`, 0 disables a rung, project-over-user scalar merge;
      pinned by `readonly_ladder_defaults_are_pinned` + `zero_disables_readonly_ladder` +
      `limits_readonly_ladder_parses_and_project_overrides_user`.)
- [x] The escalate/stop messages carry the evidence inventory (rounds, distinct resources bound,
      per-file coverage from A-28's `coverage_summary`) so the model answers from symbols instead
      of re-reading, and the user sees why the turn ended.
- [x] Existing A-20/A-28 fixtures pass unchanged; a mixed turn (reads → write → reads) never
      falsely trips (the counter resets on effect). (`effectful_dispatch_resets_readonly_ladder`,
      run on a tightened 3/5 ladder to also exercise the override path.)
- [x] Consider (and decide in the design note): default-ON per-turn token budget now that A-26
      measures cumulative billed tokens — record the decision either way. (Decision recorded in
      `docs/designs/multipass-agent-loop.md` risk 8: **stays default-OFF** — the pathological read
      case is now bounded in rounds, which is model/pricing-independent; no single token number is
      right across providers.)

## Progress
- 2026-07-03 filed from live s_356 forensics (third looping session in two days; user asked "why do
  you constantly get those"). Root cause: all shipped guards detect REDUNDANCY; none bound BREADTH.
- 2026-07-04 implemented: `guard_breadth` in `EngineLoopHost` chained after
  `guard_transcript`/`guard_resources` on the clean-round path — counts consecutive read-only
  rounds off the existing `ReadTracker` round summary (`effectful` resets, `reads == 0` is
  neutral), escalates at 6 with the full inventory (rounds, distinct resources, A-28 coverage
  spans), arms the honest `force_stop` at 10. Config: new `[limits] readonly_rounds_escalate/_stop`
  (flux-config parse + scalar merge) wired in `build_agent` via `set_readonly_ladder`. Docs: new
  "Convergence guards" section in `docs/agent-loop.md`; decision + design note appended to
  `multipass-agent-loop.md`. 4 new flux-flow tests + 1 flux-config test; full dual-workspace gate
  green.

## Notes
- Same epic as [A-20](A-20-stall-guard-resource-aware.md) / [A-28](A-28-read-coverage-stall-guard.md);
  this is the general case those two special-cased.
- The s_356 user question itself (bedrock stats show no cost) is a separate flux-cli/pricing issue —
  likely the bedrock model key vs pricing-table key mismatch (C-09's region-prefix stripping);
  reproduce and file separately.
- Design refs: multipass-agent-loop.md "Spirals are bounded, not abolished" — this story makes that
  claim true for the breadth case.
