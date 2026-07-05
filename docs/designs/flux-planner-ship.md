# flux-planner: from trained-and-usable to shippable

**Status:** designed 2026-07-05; **all non-training work SHIPPED the same day** —
flux: L-39 (multi-line strings, `"""` verbatim blocks, 1000-seed roundtrip green) +
D-53 (`flux corpus export`) implemented, full both-workspace gate green, UNCOMMITTED;
flux-model: M-11 corpus v2 done (421 base / 132 sessions / +419 paraphrase variants =
840 training rows, ALL at post-L-39 canonicalization — 345 rows multi-line; escaped
spelling measured +4.1% tokens over multi-line), M-12 data+harness done (386 repair
triples, with-one-repair metric), M-13 licenses verified (1.5B/7B apache-2.0; 3B
non-commercial), M-14 re-canonicalize done, M-15 benchmark runner done — committed
through flux-model `92775c1`. **Remaining work is exclusively training-gated**
(M-13 arms → M-12 ablation → M-14 retrain → M-15 gate → L-40 + M-16); training awaits
user approval (+ ~20 GB disk headroom for the 7B — root disk is chronically ≥99%).
· epic spanning flux (L-39, L-40, D-53) and flux-model (M-11..M-16)
**Prior art:** `docs/designs/plan-corpus-and-small-model.md` (the corpus/training initiative this
extends; its Phase 3 shipped `ollama run flux-planner` on 2026-07-04) · L-20 emission A/B
(`docs/designs/flux-lang-emission-ab.md`: json 93% vs text 60% first-emission, keep-json) ·
flux-model `docs/go-no-go.md` (Gate 1 NO-GO on compact encodings; Gate 2 NOT ship-worthy at
4/12 val compile).

## What "shippable" means (pre-registered, so results can't move it)

A fine-tuned local planner is **shippable** when ALL of:

1. **License** — Apache-2.0 base model (measured blocker: Qwen2.5-Coder-**3B** is
   `qwen-research`/non-commercial; the 1.5B and 7B Coder variants are Apache 2.0).
2. **Validity** — on a session-split val of n ≥ 40 with **3 seeds**:
   val compile ≥ **80%** greedy no-repair AND ≥ **95%** with ONE diagnostics-fed repair
   round (the production path — flux's planner loop already has that round).
3. **Comparability** — on flux's own emission-ab task suite
   (`crates/flux-eval/assets/emission-ab/tasks.json`), the fine-tuned text arm
   **decisively beats L-20's measured 60%** first-emission (the one pre-registered
   condition allowed to re-open the emission A/B; json's 93% is the reference ceiling).
4. **Served** — `ollama pull`-able (or one-command import), wired as a flux provider,
   selectable behind the kept `FLUX_EMISSION` scaffold.

## Evidence-derived strategy (what the 2026-07-04 run taught us)

- **The dominant failure is representational, not conceptual** (bf16 == q4 at 4/12):
  canonical flux text demands multi-KB escaped SINGLE-LINE JSON strings for edit
  payloads; the model emits literal newlines. Categories with short args pass
  (retry-recovery 2/2, read+grep 1/1); each-bulk-edit fails 0/4 exactly this way.
  → Attack the representation (L-39 multi-line strings) and the plan shape
  (prefer line-anchored `patch` over payload-heavy `edit` in the corpus).
- **The eligible episode pool is nearly drained** (107 left of ~407 under current
  filters, out of 3,527 episodes) — corpus growth must come from episode
  **sub-splitting** (the deferred M-03 rule; ~700 categorized episodes are blocked
  only by the ≤30-step filter), paraphrase variants, fresh sessions, and — the
  L-38 hedge — flux's own events.db (`plan_source` is released in v0.2.15).
- **Repair capability is nearly free**: 51 (broken flow, diagnostics, fixed flow)
  triples already exist from the generation runs; a repair-trained model multiplies
  with the loop's repair round (that round took Sonnet 47% → 83% on this failure class).
- **Quantization is free** (bf16 == q4_K_M) — ship the q4 artifact.
- **Packaging invariant**: ollama's native safetensors import corrupts Qwen merges —
  always via llama.cpp GGUF (`flux-model/ml/package_ollama.py` encodes this).

## Workstreams

### flux repo
- **L-39 — multi-line string literals in flux-lang.** Triple-quoted (or heredoc-style)
  string spelling; `format::format` emits it for strings containing newlines; parse
  accepts it; L-18 roundtrip property tests extended. Kills the dominant failure mode
  at the source and improves human authoring + every model's emissions (Sonnet's too).
- **L-40 — emission A/B re-run with the fine-tuned text arm.** Wire the ollama-served
  model as the text-arm planner behind `FLUX_EMISSION=text` (ollama already speaks
  Anthropic Messages as a flux provider), re-run the L-20 harness (same tasks, same
  metrics, plus with-repair numbers). Ship decision recorded against the bar above.
- **D-53 — events.db plan_source exporter.** `flux`-native corpus mining: accepted
  plans (L-38 `plan_source`) + their originating user turn → corpus rows. Continuous,
  grows with real flux usage, zero LLM cost per sample.

### flux-model repo
- **M-11 — Corpus v2 (~450 samples).** Episode sub-splitting at deterministic
  boundaries; drain the remaining pool; paraphrase variants (-vN, supported and
  unexercised); prefer-`patch` generation policy for large edits; re-balance
  categories. ~$25–35 OpenRouter.
- **M-12 — Repair-task training arm.** Mine the 51 existing triples + synthesize
  breakage (mutate valid plans, collect real diagnostics); train plan+repair as one
  model; adopt "with one repair round" as the headline production metric.
- **M-13 — Apache-licensed model arms.** 1.5B probe + **7B QLoRA** (both Apache 2.0)
  on corpus v2, 3 seeds; pick the ship candidate. 3B stays as a research comparison.
- **M-14 — Re-canonicalize + retrain after L-39.** Corpus re-renders from stored
  `ast_json` via `flux-corpus fmt` at the new flux_rev (multi-line spelling appears
  automatically in canonical text); retrain the candidate.
- **M-15 — Ship gate.** The full pre-registered bar (license/validity/comparability),
  emission-ab suite, 3 seeds, greedy + one-repair; decision mirrored here and into
  this doc.
- **M-16 — Distribution.** Publish adapter + GGUF (HF), `ollama pull`-able artifact,
  quickstart README; optionally later: signed distribution via flux's plugin
  trust-ladder (`flux model install`).

## Sequencing

Parallel-start: **M-11 + M-12 + L-39 + D-53** (independent).
Then M-13 (needs corpus v2) → M-14 (needs L-39) → M-15 (needs M-13/M-14) → L-40 + M-16
(need a candidate that passes M-15).

Estimated spend to a ship decision: ~$30–40 API, $0 GPU (local 3090; 7B via QLoRA),
roughly 3–4 focused sessions.

## Fallbacks if the bar isn't met

- Constrained decoding (GBNF) forces syntactic validity but ollama's API doesn't expose
  grammars — would require llama.cpp serving; kept as an experiment, not the ship path.
- If 7B misses 80% greedy but clears 95% with-repair: ship as "repair-loop-only" mode
  (the loop always repairs anyway) with the bar honestly restated — requires explicit
  user sign-off, not a silent goalpost move.
- If corpus v2 plateaus: the D-53 exporter compounds over time; revisit after real
  flux usage accumulates plans.
