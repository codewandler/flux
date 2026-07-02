---
id: L-20
title: Planner emission A/B — measure strict-JSON vs native-text on a fixed corpus
pillar: Language
status: ready
priority: 8
design: docs/designs/flux-lang-emission-ab.md
note: arm 1 (derived DraftAst schema on emit_plan) shipped with L-19; what remains is the actual experiment — an emission-arm selector, a fixed task corpus, and the measured comparison (acceptance rate, repair rounds, tokens) recorded back into the design doc
---

# Planner emission A/B — measure strict-JSON vs native-text on a fixed corpus

## Goal
`docs/designs/flux-lang-emission-ab.md` frames an empirical question: does the planner emit better
plans through the (now-shipped) strict derived `DraftAst` JSON schema, or through the ~3–4× more
compact native-text surface (`flux_lang::parse`/`format`)? Arm 1 shipped (L-19 status note); build
the experiment and answer it with data.

## Acceptance
- [ ] **Arm selector.** An env/flag (`FLUX_EMISSION=json|text`) switches the planner's emission
      surface without code edits; `text` mode teaches the grammar via the existing native-text
      examples and parses the emitted plan with `flux_lang::parse`. Hermetic test that both arms
      produce a runnable plan for a fixture task.
- [ ] **Fixed corpus.** A small committed corpus (10–20 planning tasks spanning the node
      working set: reads, binds, parallel, each, retry, obj/list templates) runnable offline
      against a live model behind an env gate (the flux-eval synthetic-loop precedent).
- [ ] **Measured comparison.** One run per arm on the same model
      (`openrouter-anthropic/anthropic/claude-sonnet-4.6` per the working default) reporting:
      first-emission acceptance rate, repair rounds/task, tokens/task (the C-15 efficiency
      metrics), and wall time. Results + decision recorded in the design doc's status section.
- [ ] **Decision.** The design doc's open question gets an answer: keep JSON, switch to text, or
      keep both behind the flag — with the numbers that justify it.
- [ ] Gate green for code; the corpus run itself is env-gated (spends real tokens).

## Progress
- (not started — filed 2026-07-02 during the ready-queue curation; design doc pre-exists.)

## Notes
- Anthropic API key is out of credits; use the OpenRouter Sonnet spec above for the live run.
- Keep per-arm spend bounded (~10–20 small tasks); the A-10 `--turn-budget` ceiling is available.
