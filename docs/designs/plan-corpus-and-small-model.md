# Design: plan corpus + small-model spike — canonical plan text, NL→.flux mining, compression research

## Why

Three-phase initiative (approved 2026-07-04) toward a small (1–3B) fine-tuned planner model:
(1) make every accepted plan durably available as **canonical parseable Flux-Lang text**, (2) mine
real coding-session traces into a ~100-sample NL→`.flux` training corpus, (3) measure whether a
compressed plan representation beats text syntax as a training target, then fine-tune and decide.

The original proposal wanted the production planner to *emit* text syntax. That question was
already asked and answered: L-20 built the full text-emission arm (`FLUX_EMISSION=text`) and
measured it — text lost first-emission validity 60% vs 93% (10× repair rounds) while winning token
cost ~15%; the pre-registered decision in
[flux-lang-emission-ab.md](flux-lang-emission-ab.md) is **keep `json`**. This initiative honors
that decision with a reframe:

> **Projection, not emission.** Both emission arms converge on the same `DraftAst`, and
> `flux_lang::format::format` is the total roundtrip text projection (L-18). A training corpus in
> text syntax therefore never needs the planner to emit text — any accepted plan is projected to
> canonical text deterministically. The L-20 scaffold (text arm + `FLUX_EMISSION` selector) stays:
> a future fine-tuned model that natively speaks text is the one contender that could re-open the
> A/B with a different text arm.

## Phase 1 — canonical `plan_source` on `PlanAttempted` (story L-38, this repo)

Today the accepted-plan record (`EventKind::PlanAttempted`, written in
`crates/flux-flow/src/loop_host.rs`) carries only `plan_text` = `render::render_pretty` output —
a human-audit surface that does NOT parse. The full AST is separately recorded as JSON on the
`flow.plan` observation (so history is back-fillable), but observations are capped/eventual;
`PlanAttempted` is the plan-lifecycle record.

Change (extend, don't replace):
- `plan_source: Option<String>` (serde-default, skip-if-none) on `PlanAttempted` + the
  `PlanAttempt` projection — the `format::format` projection of the accepted AST.
- Populated only on the accepted arm; **`None` on overflow rather than truncated**
  (`PLAN_SOURCE_CAP = 32_000`) — invariant: a present `plan_source` always parses. Truncation
  suffixes would poison downstream mining.
- Redacted through the same C-22 `Redactor` as `plan_text`/`error` (redaction replaces substrings
  inside string literals, so the text stays parseable).
- Back-compat: old rows decode `None` (the `phase`-field precedent); no schema bump.

Tests pin the L-18 roundtrip at the event boundary: `parse(plan_source) == accepted ast`.

## Phase 2 — NL→.flux corpus, mined from Claude Code sessions (repo: `~/projects/flux-model`)

Decision: corpus source is **Claude Code session JSONLs only** (`~/.claude/projects/**/*.jsonl`);
a flux events.db exporter is deferred (Phase 1 is the hedge that keeps flux-native mining
possible later). All corpus/ML tooling lives in the sibling repo **flux-model** (hybrid: a tiny
Rust `flux-corpus` validator bin with path-deps on flux crates + a Python pipeline), because
training tooling doesn't belong in the product workspace.

Pipeline: capture/normalize (deterministic; secret-scrub at ingest) → segment per user-prompt
(deterministic boundaries, LLM confirmation) → collapse trial/error into the optimal op trace
(Bash-argv classifier first — real traces are Bash-dominated; failed-then-succeeded keeps only
the success except in the `retry-recovery` category, where the pair IS the sample) → generate the
single-pass `.flux` plan with a Sonnet-class model prompted with the `fluxlang skill` language doc
→ validate ladder (`flux-corpus check` = parse+lower against the builtin+task catalog, one
diagnostics-fed repair round; cycle invariant `parse(canonical) == ast`; replay read-only
categories only, in ephemeral worktrees, never auto-approve) → corpus.jsonl, ~100 rows
quota-balanced over six categories, fingerprint-deduped, `flux_rev`-stamped, 15% human-reviewed.

## Phase 3 — measurement + fine-tune spike (flux-model/ml/)

"MIR" from the original proposal does not exist: the stages are `DraftAst → HirFlow →
PhysicalPlan`, and `PhysicalPlan` is runtime-internal serde-JSON with **no inverse** back to an
executable plan. So: measure first, train on what has a decode path.

- **Token measurement** across five representations (`text` = `format`, `text_compact`,
  `ast_json`, `hir_json`, `phys_json`) with the candidate models' own tokenizers.
- **Pre-registered gate:** design a custom compact encoding ONLY if the best structured repr is
  ≥30% smaller (median) than `text` AND decodable back to an executable plan. Prior expectation:
  the JSON reprs are *larger* than the DSL text.
- **Fine-tune** (LoRA, external GPU): targets `text` (primary) and `ast_json` (secondary);
  metrics = compile rate, cycle consistency, task success on the read-only subset.
- **Go/no-go** recorded in `flux-model/docs/go-no-go.md`, decision summary mirrored here.
  Ship-worthy bar: 3B compile-rate ≥80% on val AND (token saving ≥30% OR the fine-tuned text arm
  beats L-20's measured 60% first-emission decisively enough to re-open the emission A/B).

## Key files

- `crates/flux-flow/src/loop_host.rs` — accepted-plan record site (plan_source population,
  cap + redaction).
- `crates/flux-events/src/{kind,projection,store}.rs` — `PlanAttempted` event / `PlanAttempt`
  projection / `record_plan_attempt`.
- `crates/flux-lang/src/format.rs` — `format` (canonical projection), `format_compact`.
- `crates/flux-eval/tests/emission_ab.rs` — the hermetic registry + metric-harness pattern
  `flux-corpus` mirrors.
- `~/projects/flux-model` — corpus pipeline + ML spike (own tracking board there).

## Status

- 2026-07-04 — designed; L-38 filed for Phase 1; flux-model scaffold under construction.
- 2026-07-04 (later) — **Phase 1 IMPLEMENTED** (L-38, RED→GREEN: flux-events field/fold/decode +
  loop_host cap/redaction + `crates/flux-sdk/tests/plan_source.rs` e2e roundtrip); package gates
  green; full-workspace gate deferred until the concurrent stream-resilience session settles
  (its in-flight `flux-providers` edits don't compile yet — unrelated). The emission-ab design
  doc's follow-up note is resolved (scaffold kept, projection-not-emission).
- 2026-07-04 (evening) — **Phase 2 RUN + Phase 3 Gate 1 DECIDED** (details in flux-model's
  board/go-no-go): full capture over all CC projects (222k events → 3,547 segments → 3,527
  episodes), generation via Sonnet/OpenRouter with the real L-20 text grammar + live catalog
  slices (`flux-corpus catalog`) — pilot lower+cycle 10/12; the scale run yielded 47/101
  before OpenRouter credits ran out (402), salvaged by a local-ollama repair round.
  **Gate 1 = NO-GO on a compact encoding, exactly as pre-registered**: under the
  Qwen2.5-Coder-3B tokenizer, ast_json is +39.4% and hir_json +40.8% LARGER than canonical
  text (phys_json −93.5% but no inverse). The corpus trains ONE arm: canonical text.
  Fine-tune runs LOCALLY on the user's RTX 3090 (not "external GPU" as designed — the NVML
  610/595 userland/kernel mismatch broke `nvidia-smi` but NOT new CUDA contexts).
- 2026-07-04 (night) — **Phase 3 COMPLETE — model trained twice + shipped to ollama;
  Gate 2 = NOT ship-worthy**: 150-sample corpus (2nd OpenRouter batch after top-up;
  ~$15 total), Qwen2.5-Coder-3B LoRA → `ollama run flux-planner` (q4_K_M via llama.cpp
  GGUF — ollama's native safetensors import CORRUPTS the model, always convert first).
  Val compile **4/12 greedy no-repair, identical bf16 vs q4** (round 1: 2/6). Misses the
  80% bar and the L-20 60% baseline → `FLUX_EMISSION` stays closed. Dominant failure is
  REPRESENTATIONAL: literal newlines inside long single-line JSON string args (multi-KB
  edit payloads); short-arg categories pass (retry-recovery 2/2, read+grep 1/1). Path
  back to GO recorded in flux-model's go-no-go.md: corpus growth (push-button, ~$8/120
  episodes) + GBNF inference grammar or a flux-lang multi-line-string spelling + 3-seed
  re-run. Full detail: flux-model board + `runs/text-3b-r2/`.
