---
id: C-723
title: "The driver must not withhold ready work on an unverified already-built signal"
pillar: "Core"
status: backlog
priority: 1
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "A dry-run tick with 8 free slots and 9 ready items dispatched 1. C-570 and C-544 were withheld as already-built on board reconcile's say-so, yet AgentReport and safe_checkpoint do not exist in any crate, the flux-tui skill doc states the C-570 operation is unbuilt, and every C-544 hit in Rust is a doc comment forward-referencing the id. C-718 already establishes reconcile matches a mention, not an implementation; the driver consumes it as a hard block"
---

# The driver must not withhold ready work on an unverified already-built signal

## Goal

The driver's job is to keep eight workers busy. On a fleet with eight free slots and nine ready
items it dispatched one, and its reasons were wrong in both directions: it withheld two unbuilt
stories as `already-built`, and the single item it did dispatch — `exchange/X-139` — is the one
whose implementation genuinely exists, stranded on a fleet branch by [[C-721]].

`already-built` is not a fact the driver verifies. It is `board reconcile`'s heuristic, and
[[C-718]] already establishes that reconcile matches a *mention* of a story id rather than its
implementation. Withholding is the strongest action the driver takes — the item leaves the
schedulable pool silently — so it is the last place a heuristic belongs.

## Acceptance

- [ ] `already-built` never withholds on a mention. A withhold on that reason requires evidence
      tied to the story's own acceptance — the symbols, files or behaviour it names — not the
      presence of its id in prose or a doc comment.
- [ ] `C-570` and `C-544` are the fixtures, and both must dispatch. `AgentReport` and
      `safe_checkpoint` exist nowhere in `crates/`; `.agents/skills/flux-tui/SKILL.md` states the
      C-570 lifecycle report operation is unbuilt; every `C-544` hit in Rust is a doc comment in
      another module forward-referencing the id.
- [ ] A withheld item reports the concrete evidence that justified withholding it, so a wrong
      withhold is visible in the tick output rather than inferred from a shrinking dispatch count.
- [ ] When the signal is uncertain, the driver dispatches and lets the worker discover the work is
      done — a redundant turn is recoverable, a silently skipped story is not. Any reason that
      removes an item from the pool without human review fails closed toward dispatching.
- [ ] `flux fleet doctor` reports a ready item withheld across N consecutive ticks, so a permanent
      silent withhold cannot masquerade as an empty queue.
- [ ] Regression test: a tick with free slots, ready items, and a reconcile signal asserting
      `implementation-landed` for a story whose named symbols are absent dispatches that story.
