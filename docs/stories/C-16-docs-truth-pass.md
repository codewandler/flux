---
id: C-16
title: Docs truth pass — align README/vision/roadmap claims with verified reality
pillar: Core
status: ready
priority: 11
note: stale/overstated claims found by the harness review — roadmap still says "No cost tracking" (C-05/06 done) and "Two turn loops" (A-01 done); README promises "zero extra model calls" where vision says "fewest"; "argv-only" elides the bash sh -c exception; "symbols not re-sent" overstates the (growing) digest
---

# Docs truth pass

## Goal
The claims themselves are a product surface — the 2026-07-02 review graded code against them, and
several are stale or overstated. Align them with verified, landed reality (checked against code,
not aspiration). This lands LAST in the round so wording reflects the shipped fixes.

## Acceptance
- [ ] README:17 "re-running it costs zero extra model calls" → aligned with vision.md:41's
      "the fewest model calls" (a saved plan replays with zero; an agent turn does not).
- [ ] README:16 symbols claim → precise post-A-07 wording: raw outputs are stored; a **bounded**
      symbol digest is re-sent per planner call.
- [ ] README Safety model → notes the `bash` op is the documented `sh -c` exception to argv-only
      (with its defense-in-depth: subject-splitting + `<shell-expansion>` sentinel + opt-in shell
      group; `proc.run` is the argv-only alternative), and mentions the capability-scope floor as
      step 0 of the chain.
- [ ] roadmap.md "Known divergences": delete the stale "No cost tracking" entry (C-05/C-06 done)
      and the "Two turn loops" entry (A-01 done); sweep the section for other landed items.
- [ ] docs/agent-loop.md evidence-persistence note reflects C-14 (if not already updated there).
- [ ] README:170 sub-agent claim re-checked against the landed C-12 behavior (true again).
- [ ] Full gate green (docs-only, but the gate is cheap insurance); CHANGELOG entry.

## Progress
- Filed 2026-07-02 from the harness claims review (P11 of the round).

## Notes
- Verification method: every edited sentence must be traceable to landed code (file:line in the
  commit body where non-obvious).
