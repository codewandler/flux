---
id: C-16
title: Docs truth pass — align README/vision/roadmap claims with verified reality
pillar: Core
status: done
priority:
note: README/roadmap claims aligned with landed reality (scope floor, plan-intents re-fire, bash sh -c defenses, bounded digest, durable evidence, correlated sub-agent audit) + the full live verification matrix ran — 2.0 calls/turn at $0.0032, honest budget stop, durable plan text/fingerprint, correlated subagent stream, merged usage keys
---

# Docs truth pass

## Goal
The claims themselves are a product surface — the 2026-07-02 review graded code against them, and
several are stale or overstated. Align them with verified, landed reality (checked against code,
not aspiration). This lands LAST in the round so wording reflects the shipped fixes.

## Acceptance
- [x] README:17 "re-running it costs zero extra model calls" → aligned with vision.md:41's
      "the fewest model calls" (a saved plan replays with zero; an agent turn does not).
- [x] README:16 symbols claim → precise post-A-07 wording: raw outputs are stored; a **bounded**
      symbol digest is re-sent per planner call.
- [x] README Safety model → notes the `bash` op is the documented `sh -c` exception to argv-only
      (with its defense-in-depth: subject-splitting + `<shell-expansion>` sentinel + opt-in shell
      group; `proc.run` is the argv-only alternative), and mentions the capability-scope floor as
      step 0 of the chain.
- [x] roadmap.md "Known divergences": delete the stale "No cost tracking" entry (C-05/C-06 done)
      and the "Two turn loops" entry (A-01 done); sweep the section for other landed items.
- [x] docs/agent-loop.md evidence-persistence note reflects C-14 (if not already updated there).
- [x] README:170 sub-agent claim re-checked against the landed C-12 behavior (true again).
- [x] Full gate green (docs-only, but the gate is cheap insurance); CHANGELOG entry.

## Progress
- Filed 2026-07-02 from the harness claims review (P11 of the round).
- Done 2026-07-02. README: "zero extra model calls" → stored-plan replay = zero, live turn =
  fewest; symbols → "bounded digest of summaries" (A-07); safety chain now shows the
  capability-scope floor as step 0, the plan-intents disclosure re-fire (C-12), the `bash` sh -c
  exception with its real defenses (per-token subjects + `<shell-expansion>` sentinel subject +
  destructive heuristics; verified at flux-tools lib.rs:995/:1040), redactor coverage incl.
  program secrets (C-13), durable evidence + plan attempts (C-14), and correlated sub-agent audit
  (A-08). roadmap: "Two turn loops" and "No cost tracking" struck as done (A-01, C-05/06/15).
  agent-loop.md was already fixed in C-14; vision.md verified accurate as written.
- **Live verification matrix** (openrouter sonnet, real events.db):
  - A-06/C-15: a completion-carrying turn made exactly **2.0 calls/turn** (planner + toolless
    render), grounded closing text, **$0.0032 / ~3s** vs the A-05 baseline $0.0137 — and the
    `flux usage` efficiency line reports it.
  - A-10: `--turn-budget 1` ran the plan then ended with the honest budget message ("budget (1
    tokens) is exhausted (31817 used)").
  - C-14: events.db carries `plan_attempted` (accepted, rendered plan text + fingerprint) and
    `observation` events (tool_call, turn.iteration, groups.active with `signals:["kubernetes"]`).
  - A-08: a live `task worker` delegation created stream s_331 with `agent_id=subagent:worker`,
    `correlation_id=s_330` (parent), its own full durable trail, and a `subagent.trace` pointer on
    the parent.
  - C-15: all-sessions `gpt-5.5` + `openai/gpt-5.5` merged into one 70-call row (after the
    cross-stream merge fixup this check surfaced).
  - C-12: no destructive op is reachable live in a default workspace (bash off, no rm builtin) —
    the deny is proven hermetically on the real emit_plan path
    (`sub_agent_denies_destructive_plan_from_emit_plan` + the two scope-disclosure tests).

## Notes
- Verification method: every edited sentence must be traceable to landed code (file:line in the
  commit body where non-obvious).
