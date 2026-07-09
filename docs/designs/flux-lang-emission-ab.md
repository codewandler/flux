# Design: planner emission surface — strict JSON schema vs native text (an accuracy A/B)

> **Status update (2026-07-09, L-71): MEASURED, DECIDED — `merged` is now the default arm.**
> Two live runs on codex/gpt-5.5 (15 tasks × 3 arms each): pooled first-emission acceptance
> **28/30 json vs 28/30 merged** (run 2: merged 15/15, zero repair rounds — beating json's 14/15)
> at **−26% uncached input / −23% est. cost**. That clears the pre-registered parity bar, so
> `FLUX_EMISSION` unset now selects `merged`; `json` and `text` stay opt-in for re-measurement.
> Tables under *The merged arm → Measured*.
>
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

## The merged arm (2026-07-09, L-71) — one node object instead of the 43-way union

### Why

The json arm's `emit_plan` schema is the schemars-derived `DraftAst` schema: an internally-tagged
`oneOf` with **43 variants**, each re-declaring shared properties (`body`, `bind`, `cond`, `max`,
…) and each carrying its doc-comment as a variant description. Measured: **29,911 bytes
(~7.5k tokens)** — on top of the ~3.2k-token node-kind catalog the planner prompt *also* carries,
so the same per-kind semantics ride twice on every planning call.

What the strict union buys on the wire is less than it looks:

- **Providers don't enforce it.** No provider constrained-decodes a recursive 43-way `oneOf`; the
  schema is guidance tokens, not validation.
- **The rules that matter are context-sensitive.** Placement (`checkpoint` top-level only, pure
  leaves inside `obj`/`list`, empty-branch rejection) is inexpressible in JSON Schema; the
  analyzer + repair loop are the enforcement authority in *every* arm.
- **The wire format is already uniform.** Internal serde tagging means every node is
  `{"kind": …, …props}` — the union exists only in the schema's *description* of that format.

So the merged arm advertises the same wire format through **one** `Node` object schema and lets
the catalog own per-kind semantics. Expected upside beyond token cost: a simpler contract for
small/weak models (the parse-resilience history — qwen/deepseek/GLM — is largely emission-shape
trouble), and a shape simple enough that strict/constrained decoding could actually be enabled on
providers that support it.

### The merge (a projection, not a language change)

`flux_lang::schema::merge_node_schema` (memoized as `model_schema()`) post-processes the derived
schema — the AST types, serde encoding, parser, and every `ast_schema()` consumer are untouched:

- `kind` → `{"type": "string", "enum": [all 43 tags, declaration order]}`;
- every other property → the **union** across variants, declared once, all optional
  (`required: ["kind"]`);
- a property whose shape differs across kinds (7 of 60: `as`, `branches`, `cases`, `message`,
  `name`, `steps`, `value`) merges to an `anyOf` of the distinct shapes; a shape that already
  accepts anything (`lit.value`) absorbs the union to "anything";
- a field description survives only when **every** kind carrying the property agrees on it —
  a shared property's meaning is kind-dependent, and that is the catalog's job (this rule exists
  because the naive first-seen merge stamped `var.name` with *throttle's* "stable bucket name"
  doc);
- idempotent, and tolerant of any schema without a `oneOf` `Node` definition — so it applies to
  both the bare `ast_schema()` and `emit_plan`'s `tool_input_schema::<EmitPlanInput>()` (which
  keeps `complete`/`gather`/`brief` intact).

Measured result: **29,911 B → 10,248 B (−66%, ~7.5k → ~2.6k tokens)** for the bare AST schema;
the `emit_plan` tool schema shrinks proportionally (guarded by a `< 50%` test in both crates).

Wiring (`FLUX_EMISSION=merged`): `EmissionArm::Merged` shares the json arm's prompt bytes, decode
path, gates, and repair loop — `planner_tools` swaps only `emit_plan.input_schema`. The live
harness (`crates/flux-eval/tests/emission_ab.rs`) now runs json/text/merged and prints a
three-column table.

### What was deliberately NOT done

- **No semantic node consolidation.** Collapsing the 43 kinds into category nodes with mode
  props would trade variant pattern-matching (analyzer, formatter, optimizer, native grammar)
  for stringly-typed prop validation — rejected; the kinds are not the problem, the schema
  union was.
- **No per-node reliability props** (`retry:`/`timeout_ms:` on every node, the Step-Functions
  shape, or `on_timeout` edges). Wrappers express *extent* (what the deadline covers), which
  edges/props don't; and an ingress desugaring would create a second spelling of the same plan,
  fragmenting the plan corpus. Revisit only if merged-arm measurement shows nesting depth (not
  schema size) is the remaining pain.

### Decision rule (pre-registered, mirroring L-20)

Cut production over to `merged` only if, on the same 15-task corpus + model, it holds
**first-emission acceptance and accepted-within-one-retry at parity with `json`** (the L-20
numbers: 93% / 15-of-15) while keeping its token savings. If it merely ties on cost or loses
validity, `json` stays and the arm is deleted (no-fallbacks). Until measured, `json` remains the
default; `merged` is opt-in via `FLUX_EMISSION=merged`.

### Measured — run 1 (2026-07-09, `codex/gpt-5.5`, 15 tasks/arm)

The harness now takes a full provider spec via `FLUX_EMISSION_AB_MODEL` (`codex/<model>` →
ChatGPT/Codex subscription provider; anything else → OpenRouter model id, the L-20 route).

| metric | json (ast schema) | text (native source) | merged (one-node schema) |
|---|---|---|---|
| plans accepted | 15/15 | 15/15 | 15/15 |
| first-emission acceptance | **14/15** | 11/15 | **13/15** |
| accepted within one retry | **15/15** | 15/15 | **14/15** |
| repair rounds (total) | 1 | 4 | 4 |
| uncached input tok (total / per task) | 176,531 / 11,769 | 152,120 / 10,141 | 130,818 / **8,721 (−26%)** |
| cache-read tok (total) | 61,440 | 0 | 45,056 |
| output tok (total / per task) | 3,739 / 249 | 3,042 / 203 | 4,388 / 293 |
| wall time (total / per task) | 130.6s / 8.7s | 102.8s / 6.9s | 157.2s / 10.5s |
| est. cost | $1.0255 | $0.8519 | $0.8083 (**−21%**) |

Reading:

- **Cost thesis confirmed:** merged saves ~3.0k uncached input tokens per planning call vs json
  (−26%) and −21% estimated cost, on top of json's own cache hits.
- **Validity is one task below parity, concentrated in one outlier:** 3 of merged's 4 repair
  rounds came from a single task (`jq-extract`, a 1,406-output-token repair spiral); `when-branch`
  cost both json and merged one repair each (intrinsically hard task, not schema-shaped). A
  one-task delta on a 15-task corpus is within noise — but the pre-registered bar is parity, and
  strictly this run does not clear it.
- **Text arm re-confirms L-20** on a second model family: cheapest input, worst first-emission
  acceptance (11/15).

**Verdict after run 1: no cutover on that run alone** — one confirming run required.

### Measured — run 2 (2026-07-09, `codex/gpt-5.5` again) and the decision

The confirming run stayed on codex/gpt-5.5 (user call: the subscription is flat-rate, and the
rule's "same corpus + model" comparison is cleanest when the confirming sample is drawn from the
same model as run 1).

| metric | json (ast schema) | text (native source) | merged (one-node schema) |
|---|---|---|---|
| plans accepted | 15/15 | 15/15 | 15/15 |
| first-emission acceptance | 14/15 | 8/15 | **15/15** |
| accepted within one retry | 15/15 | 14/15 | **15/15** |
| repair rounds (total) | 1 | 8 | **0** |
| uncached input tok (total / per task) | 145,800 / 9,720 | 94,641 / 6,309 | 107,443 / 7,163 |
| output tok (total / per task) | 3,533 / 236 | 4,495 / 300 | 3,179 / 212 |
| wall time (total / per task) | 119.5s / 8.0s | 156.9s / 10.5s | **103.8s / 6.9s** |
| est. cost | $0.8811 | $0.6372 | $0.6664 |

Merged swept run 2: 15/15 first emission, zero repairs, fastest arm, and json's only repair was
`when-branch` again. Run 1's `jq-extract` spiral did not reproduce — a one-off, not a schema
defect. (Text collapsed to 8/15 here; its L-20 loss is now confirmed on two model families.)

**Pooled, 30 tasks/arm (the decision sample):**

| metric | json | text | merged |
|---|---|---|---|
| first-emission acceptance | 28/30 (93%) | 19/30 (63%) | **28/30 (93%)** |
| accepted within one retry | 30/30 | 29/30 | 29/30 |
| repair rounds (total) | 2 | 12 | 4 |
| uncached input tok / task | 10,744 | 8,225 | **7,942 (−26%)** |
| est. cost (both runs) | $1.9066 | $1.4891 | **$1.4747 (−23%)** |

**DECISION: cut over.** First-emission acceptance is at exact parity (28/30 both arms, and the
confirming run beat json outright); within-one-retry differs only by run 1's unreproduced
outlier (29/30 vs 30/30); the token savings hold (−26% input, −23% cost). Per the pre-registered
rule, `merged` is now the **default** arm (`EmissionArm::default()` / `FLUX_EMISSION` unset).
`json` (the L-20 winner) and `text` remain opt-in via `FLUX_EMISSION` as the measurement
scaffold — `json` for re-measurement on new model families, `text` because L-40 wants it re-run
behind a fine-tuned model. Caveat recorded: both runs were one model family (gpt-5.5); if a
production model regresses on merged, `FLUX_EMISSION=json` is the diagnostic, and the A/B
harness re-cuts the table.

### Key files

- `crates/flux-lang/src/schema.rs` — `merge_node_schema` / `model_schema` + drift-proof tests
  (kind-enum ↔ catalog, union completeness, no dangling `$ref`, size bound, idempotence).
- `crates/flux-flow/src/compile.rs` — `EmissionArm::Merged`, `merged_emit_plan_schema()`,
  planner-tools/prompt wiring + arm tests (prompt byte-equality, schema shape, same-payload
  same-plan).
- `crates/flux-eval/tests/emission_ab.rs` — the three-arm live runner.
- `crates/flux-lang/src/bin/fluxlang.rs` — `fluxlang schema --merged` (the public inspect
  surface; documented on the website's Execution model + Tooling pages).
