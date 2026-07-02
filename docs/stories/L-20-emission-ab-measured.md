---
id: L-20
title: Planner emission A/B — measure strict-JSON vs native-text on a fixed corpus
pillar: Language
status: done
priority: 8
design: docs/designs/flux-lang-emission-ab.md
note: MEASURED, DECIDED — keep json: strict-schema wins first-emission acceptance 93% vs 60% and within-one-retry 15/15 vs 13/15; native text wins base-prompt size (−47%) and cost (~15%) but loses validity + wall time (+10%); 15-task corpus × 2 arms live on openrouter sonnet ($1.74 total); FLUX_EMISSION selector stays (json default byte-identical, text behind the flag); full table in the design doc
---

# Planner emission A/B — measure strict-JSON vs native-text on a fixed corpus

## Goal
`docs/designs/flux-lang-emission-ab.md` frames an empirical question: does the planner emit better
plans through the (now-shipped) strict derived `DraftAst` JSON schema, or through the ~3–4× more
compact native-text surface (`flux_lang::parse`/`format`)? Arm 1 shipped (L-19 status note); build
the experiment and answer it with data.

## Acceptance
- [x] **Arm selector.** An env/flag (`FLUX_EMISSION=json|text`) switches the planner's emission
      surface without code edits; `text` mode teaches the grammar via the existing native-text
      examples and parses the emitted plan with `flux_lang::parse`. Hermetic test that both arms
      produce a runnable plan for a fixture task.
- [x] **Fixed corpus.** A small committed corpus (10–20 planning tasks spanning the node
      working set: reads, binds, parallel, each, retry, obj/list templates) runnable offline
      against a live model behind an env gate (the flux-eval synthetic-loop precedent).
- [x] **Measured comparison.** One run per arm on the same model
      (`openrouter-anthropic/anthropic/claude-sonnet-4.6` per the working default) reporting:
      first-emission acceptance rate, repair rounds/task, tokens/task (the C-15 efficiency
      metrics), and wall time. Results + decision recorded in the design doc's status section.
- [x] **Decision.** The design doc's open question gets an answer: keep JSON, switch to text, or
      keep both behind the flag — with the numbers that justify it.
- [x] Gate green for code; the corpus run itself is env-gated (spends real tokens).

## Progress
- **Done (2026-07-02).** Built, measured, decided:
  - **Selector:** `EmissionArm {Json (default), Text}` from `FLUX_EMISSION`, read in `compile_turn`
    (the single front door — engine/loop-host/CLI inherit with zero threading); unset/json is
    byte-identical to the shipped surface (asserted); invalid values hard-error.
    `compile_turn_with_arm` lets the runner drive both arms in one process.
  - **Text arm:** `emit_plan` takes one native-text `source` string; worked examples derived from
    the JSON grammar's own examples via `flux_lang::format` (in-sync-by-construction,
    round-trip asserted); the decoded AST flows through the SAME A-04 hidden-op + C-17
    analyze/lower gates (hermetically proven, incl. hidden-op-on-final-step rejection).
  - **Corpus:** 15 committed tasks spanning reads/binds, parallel, each(+collect), retry, repeat,
    when, obj/list templates, fmt/jq/parse, write/edit, assert; hermetic `corpus_is_valid` +
    env-gated live runner (`FLUX_EMISSION_AB=1 … -- --ignored`).
  - **Measured** (openrouter sonnet 4.6, 15 tasks/arm, serial): json 14/15 first-emission (93%),
    1 repair round, ~18.0k base prompt tok/call, 87.2s, ~$0.94 · text 9/15 (60%), 10 repair
    rounds, ~9.6k tok/call (−47%), 96.0s, ~$0.80. Within one retry: json 15/15, text 13/15.
    Text repair hotspots: when-branch ×3, jq-extract ×3.
  - **Decision (recorded in the design doc): keep `json`.** The pre-registered tie-break required
    text to win BOTH validity-within-one-retry AND cost; it wins cost, loses validity and wall
    time. Follow-up noted: delete the scaffold or re-measure after adding when/retry text
    examples if the cost edge is worth chasing.
  - Total live spend ≈ $1.74. flux-eval gained flux-provider/flux-providers as dev-deps only.

## Notes
- Anthropic API key is out of credits; use the OpenRouter Sonnet spec above for the live run.
- Keep per-arm spend bounded (~10–20 small tasks); the A-10 `--turn-budget` ceiling is available.
