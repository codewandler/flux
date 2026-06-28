# Design: planner emission surface — strict JSON schema vs native text (an accuracy A/B)

## Why

The planner emits its execution plan (a `DraftAst`) through the `emit_plan` tool. Today it advertises
that plan as **deeply-nested JSON against a loose schema**: `planner_tools()` declares
`"ast": { "type": "object" }` (`crates/flux-flow/src/compile.rs`, ~line 731) — the model gets **no
structural guidance**, and every step is a 5–10 line nested object (`{"kind":"bind","value":{"kind":
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

**Not built.** Prerequisite (native-text spellings) shipped (`8052796`). This doc is the spec; an
impl plan belongs in `.flux/plans/` when the work starts.
