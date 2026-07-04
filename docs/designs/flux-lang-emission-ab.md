# Design: planner emission surface — strict JSON schema vs native text (an accuracy A/B)

> **Status update (2026-07-02, L-20): the A/B has been RUN — decision: keep `json`.** See
> *Measured results & decision* at the bottom of this doc.
>
> **Status update (2026-07-02, L-19):** arm 1 — the strict derived `DraftAst` schema on `emit_plan` —
> has since **shipped**: `planner_tools()` now advertises `tool_input_schema::<EmitPlanInput>()`
> (`crates/flux-flow/src/compile.rs`), whose `ast` field is the schemars-derived `DraftAst` schema.
> The loose `{"type":"object"}` described below no longer exists. What remains unbuilt is the
> **measured A/B** against the native-text arm (the selector, corpus run, and comparison table).

## Why

The planner emits its execution plan (a `DraftAst`) through the `emit_plan` tool. At the time this
was written it advertised that plan as **deeply-nested JSON against a loose schema**:
`planner_tools()` declared `"ast": { "type": "object" }` — the model got **no structural guidance**,
and every step is a 5–10 line nested object (`{"kind":"bind","value":{"kind":
"call","op":…,"args":[{"kind":"lit",…}]}}`).

There is also a **native text surface** (`flux_lang::parse`/`format`) that is ~3–4× more compact for
the same logic and now covers the planner's full working set — after the P8 work the bolted-on
long-tail (`obj`/`list` templates, `assert`, `retry`, `parallel`) spells natively too, so a native
planner no longer falls back to `@json` for the nodes the prompt recommends.

Two plausible improvements compete, and which one wins is an **empirical** question, not one to guess:

1. **Tighten the JSON** — replace the loose `{"type":"object"}` with the real derived schema. `Node`
   and `DraftAst` already `derive(JsonSchema)` (`crates/flux-lang/src/ast.rs`) and `schemars` is a
   workspace dep, so `schemars::schema_for!(DraftAst)` drops in. A tight schema shown to the model
   (and enforced where the provider supports it) should lift JSON accuracy with near-zero cost.
2. **Switch to native text** — advertise a `source` string and parse it with `flux_lang::parse`. Far
   fewer tokens and a surface that matches how the model "thinks" — but it asks the model to emit an
   indentation-sensitive bespoke DSL it learns from a short in-prompt grammar, which may *lower*
   first-attempt validity.

This design sets up a **measured A/B** between the two, then a **clean cutover to the winner** (no
permanent dual path — honoring the project's no-fallbacks stance). The native-text spellings are the
prerequisite that makes arm (2) a *fair* contender; they are already shipped (`8052796`).

## The two arms (a temporary measurement scaffold)

A config/CLI selector — `plan_surface = json | text` (`crates/flux-config`, threaded to
`compile.rs`) — chooses how `emit_plan` is advertised and parsed in `compile_turn` (compile.rs
~271–324), the single front door every planner turn flows through (the engine's own agent-loop
re-enters it via `loop_host.rs`).

- **`json` (control).** `planner_tools()` advertises `"ast"` with `schemars::schema_for!(DraftAst)`
  instead of `{"type":"object"}`. Parsing stays `serde_json::from_value::<DraftAst>`. *Caveat:* `Node`
  is a large internally-tagged (`kind`) enum, so schemars emits a `$ref`/`oneOf`-heavy schema;
  provider enforcement of that varies — it always *guides* the model, but strict-mode providers (e.g.
  OpenAI) may need light massaging (`additionalProperties:false`, all-required). Even un-enforced,
  showing the model a tight schema is expected to beat the current loose object.
- **`text` (treatment).** `planner_tools()` advertises `"source": { "type": "string" }`; the handler
  parses via `flux_lang::parse::parse` (total — malformed input returns `FlowError::Parse`, never a
  panic). The prompt teaches a compact native grammar block plus 2–3 worked examples **rendered by
  `flux_lang::format::format`** (so they are guaranteed parseable and in-sync via the round-trip
  invariant); per-kind descriptions keep coming from the `schema::node_kind_catalog()` SSOT.

**Parse-failure → retry** reuses the existing machinery unchanged: a bad payload pushes an `is_error`
`tool_result` (compile.rs ~319–323) and the `for step in 1..=max_steps` loop re-prompts. Only the
error string differs per arm. On the final step a hard failure exits with the same "no plan within N
steps" path as today's bad-JSON case.

## The A/B: metrics & corpus

Two kinds of accuracy, kept distinct:

- **Syntactic (cheap proxy):** first-attempt `parse` + `analyze` success rate, turns-to-valid-plan
  (retry depth), and `emit_plan` input tokens. Run over a fixed set of ~15–20 representative NL task
  prompts.
- **Semantic (the real gate):** does the emitted plan *do the right thing*? Measure task pass-rate on
  a handful of executable **terminal-bench** tasks via the existing eval adapter (the only arm that
  runs the plan end-to-end). Syntactic validity ≠ a correct plan; the proxy only explains *why* an arm
  wins.

The model side needs a live provider (the one non-offline part) — per project convention use
`-m openrouter-anthropic/anthropic/claude-sonnet-4.6`. Hold model + temperature + task set fixed
across arms. Wiring/parse-path tests use the offline Mock provider (`compile.rs` test helpers).

## Decision & cutover

Ship the winner via a **clean cutover** — replace the surface, delete the loser's path and its
prompt/grammar, drop the `plan_surface` selector. Tie-break toward `json` (it's the model's
best-supported output mode) unless `text` clearly wins on both validity-within-one-retry **and**
token cost. Note: the strict-schema `json` arm is a **net improvement over today's loose schema
regardless**, so even a "native loses" outcome leaves a shippable win.

## Risk (why measure-first, not a blind ship)

- The change is **global** — every planner turn and the engine's own agent-loop flow through
  `compile_turn`.
- The model is heavily tuned to emit JSON tool inputs; a bespoke, indentation-sensitive DSL learned
  from a short grammar is inherently riskier, and `parse.rs` is stricter than serde (tabs forbidden,
  exact keyword placement). Expect a higher *first-attempt* reject rate before it falls — the retry
  loop absorbs it at a token/latency cost, capped at `max_steps`.
- Mitigation is the spike itself: build both arms behind the selector, measure on the corpus above,
  and let the data pick. If native loses mainly on a specific construct, that construct's native
  spelling/grammar is the fix, then re-measure.

## Key files

- `crates/flux-flow/src/compile.rs` — `planner_tools()` (the `ast`→strict-schema / `source`-string
  swap), the `emit_plan` handler in `compile_turn` (parse dispatch + per-arm error string),
  `build_planner_prompt()`/`ast_grammar()` (native grammar block + `format()`-rendered examples).
- `crates/flux-lang/src/ast.rs` — `Node`/`DraftAst` `JsonSchema` derives (source of the strict schema).
- `crates/flux-lang/src/parse.rs`, `format.rs` — `parse`/`format` (the `text` arm; round-trip
  guarantees in-sync examples).
- `crates/flux-lang/src/schema.rs` — `node_kind_catalog()` SSOT (per-kind descriptions for the prompt).
- `crates/flux-config/src/lib.rs` — the `plan_surface` selector.
- `crates/flux-eval` — the terminal-bench adapter used for the semantic metric.
- A small A/B harness (script under `bench/` or a `flux-eval` mode) that runs the corpus through each
  arm and emits the comparison table.

## Verification

- **Offline (Mock provider):** a prompt-grammar test asserting each embedded `text`-arm example
  `parse`s + `analyze`s clean (and `format(parse(example)) == example`); an engine test that a native
  `{"source": …}` `emit_plan` payload becomes the expected `DraftAst` and runs; a parse-error→retry
  test (mirrors the existing `plan_repairs_an_invalid_emit`); the `json`-arm test that
  `schema_for!(DraftAst)` is advertised and a schema-shaped payload parses.
- **Gate:** `cargo build/test/clippy -D warnings/fmt` for `flux-flow` + `flux-lang` + `flux-config`,
  plus `cargo test -p flux-codegate`. Regenerate `crates/flux-lang/skill/SKILL.md` only if the
  planner-facing SSOT output changes.
- **Live (the actual A/B):** the corpus run on OpenRouter Sonnet producing the syntactic + semantic
  comparison table; the cutover lands only after the table is reviewed.

## Status

**Both arms built and measured (L-20); decision below.** The strict-schema `json` arm had landed
with L-19; L-20 added the selector (`FLUX_EMISSION=json|text`, default `json`, byte-identical
surface when unset), the `text` arm (`emit_plan` takes a `source` string; native grammar block with
worked examples derived from the JSON grammar's own examples via `flux_lang::format::format`, so
the two arms teach byte-equivalent plans; parsed with `flux_lang::parse` and gated by the SAME
hidden-op + analyze/lower checks), a fixed 15-task corpus
(`crates/flux-eval/assets/emission-ab/tasks.json`), and the env-gated live runner
(`crates/flux-eval/tests/emission_ab.rs`; `FLUX_EMISSION_AB=1 … -- --ignored --nocapture`). The
selector was named `plan_surface` in this doc's sketch; it shipped as the `FLUX_EMISSION` env
switch read in `compile_turn` — the single front door — so engine/loop-host/CLI all inherit it.

## Measured results & decision (2026-07-02, L-20)

One run per arm over the 15-task corpus, same conversation/options per task, serially, against
`openrouter-anthropic/anthropic/claude-sonnet-4.6` (OpenRouter Anthropic-Messages wire; no prompt
caching was returned — `cache_read`/`cache_write` = 0 in both arms, so every call paid fresh
input). Full builtin op catalog (37 ops), no session view, `max_steps = 4` (≤3 repair rounds),
default temperature. Metrics straight from the planner's `Usage` + `Compiled::attempts`.

| metric | json (strict `ast` schema) | text (native `source`) |
|---|---|---|
| plans accepted | 15/15 | 15/15 |
| **first-emission acceptance** | **14/15 (93%)** | **9/15 (60%)** |
| repair rounds (total) | 1 | 10 |
| accepted within one retry | 15/15 | 13/15 |
| prompt size (base, tokens/call) | ~18.0k | ~9.6k (−47%) |
| final-call input tok (total / per task) | 270,919 / 18,061 | 146,084 / 9,739 |
| billed input est. incl. repair calls¹ | ~289k | ~242–247k |
| output tok (total / per task) | 5,028 / 335 | 4,114 / 274 |
| wall time (total / per task) | 87.2s / 5.8s | 96.0s / 6.4s |
| est. cost @ $3/M in + $15/M out | ~$0.94 | ~$0.79–0.80 |

¹ `Usage::accumulate` keeps only the final call's prompt occupancy per turn; each repair round adds
one earlier billed call, estimated here between the arm's base prompt (lower bound) and the final
prompt (upper bound). The undercount is identical in kind across arms; it penalizes the arm with
more repairs (text).

Per-construct repair hotspots — text arm: `when-branch` ×3 and `jq-extract` ×3 (both accepted only
on the 4th attempt), `retry-tests`/`repeat-append`/`write-then-read`/`grep-assert` ×1 each. Json
arm: `jq-extract` ×1 (the one task that also cost json a repair — jq/parse semantics, not the
surface). The text failures cluster on multi-clause statements (`when`/`else` bodies, `retry`
headers) and the parser's strictness (indentation, statement-position rules) — exactly the risk
this doc predicted for an in-prompt-taught DSL.

**Decision: keep `json`.** The pre-registered tie-break said: cut to `text` only if it clearly wins
on **both** validity-within-one-retry **and** token cost. It wins token cost (~15% cheaper overall
— the −47% prompt and −18% output more than absorb 10× the repair rounds) but **loses validity**
(60% vs 93% first-emission; 13/15 vs 15/15 within one retry) and wall time (+10%). Emission
validity is the planner's core reliability property — every repair round is user-visible latency
and a chance to exhaust the step budget — so `json` stays the production surface and the default.

Cutover note (the no-fallbacks stance): the `FLUX_EMISSION` selector + text arm are a measurement
scaffold, not a product feature. Follow-up after this table is reviewed: either delete the text arm
and selector outright, or first re-measure with the construct-level fixes the hotspots suggest
(e.g. a `when`/`retry` worked example in the text grammar) if the ~15% cost edge is judged worth
chasing. The semantic (terminal-bench pass-rate) leg was not run — both arms produce the same
`DraftAst`s once accepted, and the syntactic gap was decisive under the pre-registered rule.

**Follow-up resolved (2026-07-04):** the scaffold is deliberately **kept** (neither arm deleted,
no re-measure now). The [plan-corpus-and-small-model](plan-corpus-and-small-model.md) initiative
reframes the text question as *projection, not emission* — corpus/training text comes from
`format::format` over accepted `DraftAst`s, so production keeps `json` per the table above. The
one contender that could re-open this A/B is a fine-tuned small model natively speaking text (its
first-emission validity would not be limited by in-prompt grammar learning); the text arm +
selector stay as the ready-made harness for that future re-measure.
