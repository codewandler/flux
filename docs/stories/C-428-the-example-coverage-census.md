---
id: C-428
title: "Half the language has no example — `agent_loop`, `await`, `datasource` and eleven more appear in zero files"
pillar: Core
status: ready
priority: 12
design: docs/designs/flux-recipes.md
epic: flux-recipes
areas: [examples, flux-lang, docs]
note: "measured 2026-08-01 across the 16 files in examples/: agent_loop 0, await 0, datasource 0, agent 0, checkpoint 0, memo 0, throttle 0, debounce 0, risk 0, try/catch/finally 0, race 0, match 0. journey/trigger/channel appear once each — all in channels-app.flux. ⚠ The durability vocabulary is what separates flux from a chat loop, and it is undemonstrated"
---

# The vocabulary that makes flux different is the vocabulary with no examples

## Goal

Make example coverage a measured, repeatable property instead of an impression — and classify every
gap as *needs a recipe*, *covered by docs*, or *deliberately unshown*.

## What a first-pass sweep found

Across the 16 files in `examples/`, counting files whose source uses each keyword at statement
position:

| surface | files |
|---|---|
| `agent_loop`, `await`, `datasource`, `agent`, `checkpoint`, `memo`, `throttle`, `debounce`, `risk`, `try`/`catch`/`finally`, `race`, `match` | **0** |
| `journey`, `trigger`, `channel`, `route`, `saga`, `step`, `once`, `verify` | **1** each |
| `timeout`, `retry`, `confirm` | 2 |
| `repeat` | 4 |
| `each`, `loop`, `parallel`, `branch`, `budget` | 4–6 |

⚠ **The zeros are not a random sample.** `agent_loop` is the adaptive agent loop — a headline
capability. `await` is durable suspension; a journey that suspends on an event and resumes later with
no model re-spend is arguably flux's single most distinctive behaviour against a chat-loop agent, and
**no example shows it**. `try`/`catch`/`finally` and `race` are the resilience vocabulary. The
app-shaped surface — `channel`, `trigger`, `journey` — is carried by exactly one file.

The corpus is CI-gated and honest about being a language demonstration. The finding is not that it is
bad; it is that **the parts hardest to appreciate from a grammar table are the parts with nothing to
run.**

## Acceptance

- [ ] ⚠ **The numbers above are confirmed against the grammar, not against a grep, before anything is
      built on them.** This first pass matched keywords at statement position and could easily
      over- or under-count — an alternate spelling, a keyword only legal in a nested position, or a
      construct spelled differently in the canonical dialect would each skew it. The repo's own
      cautionary case is C-406's audit regex, which produced 319 phantom findings. **Treat this table
      as a lead, not as evidence, until this box is ticked.**
- [ ] The census ships as a **repeatable check** — a script or test — not a table in a story that is
      stale within a month.
- [ ] Every zero is classified: *needs a recipe* · *covered by docs* (with the link) · *deliberately
      unshown* (with the reason). ⚠ Do not silently treat "covered by docs" as equivalent to a runnable
      example; the epic exists because they are not the same thing.
- [ ] The gaps that get recipes are filed as stories under this epic, prioritized by how distinctive
      the capability is rather than by how easy it is to demonstrate. `await` and `agent_loop` are the
      ones that matter.
- [ ] ⚠ **Decide whether the census becomes a gate.** A check that fails CI when a new keyword lands
      with no example is a real forcing function and also a real tax on language work — L-102's syntax
      simplification is actively removing spellings, and a census gate could fight it. Decide, and say
      which.
- [ ] Full gate green.

## Notes

- Independent of the rest of the epic: this can run before, during or after the flagship. It is `ready`
  for that reason.
- The keyword list is in `crates/flux-lang/src/ast.rs` (the reserved-name table, ~line 100 onward) —
  that is the authoritative source to census against, not a hand-written list.
- ⚠ Interacts with **L-102 flux-syntax-simplification**, which is deliberately *removing* legacy
  spellings. Census the canonical dialect, or the report will demand examples for spellings that are
  being deleted.
- Coverage is not the goal — a recipe per keyword would be exactly the language-sample corpus this
  epic is reacting against. The goal is that no *distinctive capability* is undemonstrable.

## Progress

- Filed 2026-08-01 with the flux-recipes epic, from a keyword sweep of `examples/`.
