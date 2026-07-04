# Design: Planner parse resilience

**Status:** implemented 2026-07-03 (4/4 stories done, failing-first tests at the codec, compile and loop-host levels, full gate green; A-30 live-verified — the exact s_360 model+prompt now plans on the first emission) · **Pillar:** Agent · **Stories:** [A-30](../stories/A-30-stringified-ast-fallback.md) · [A-31](../stories/A-31-planner-reject-surfacing.md) · [C-31](../stories/C-31-planner-usage-on-error.md) · [A-32](../stories/A-32-openai-wire-tool-args-resilience.md)

Wave 2 — the SSE **envelope** layer this epic never touched, plus structural enforcement — is
[stream-resilience](stream-resilience.md) (filed 2026-07-04; includes the `FLUX_PLANNER_TRACE`
residual below as A-38).

## Why

Session `s_360` (2026-07-03): `flux run -m openrouter/qwen/qwen3.7-max` died on its first turn with
the bare error *"planner did not produce a plan within 8 steps"*. Live repro with temporary
instrumentation (s_361) root-caused it: **qwen3.7-max double-encodes `emit_plan`'s `ast` argument** —
it passes `{"ast": "<JSON-encoded string>"}` instead of `{"ast": {…object…}}`. flux's strict
`serde_json::from_value::<DraftAst>` rejects the string ("invalid type: string … expected struct
DraftAst"), the model re-emits the same shape on every repair step, and the turn burns the full step
budget. The plans **inside** the string were well-formed, sensible gather DAGs — the loss is pure
encoding. qwen3.7-plus shows the same behavior (s_358); GLM 5.2 is a sibling of the same genre
(malformed emit_plan JSON, 2026-06-30); Sonnet on the same OpenRouter wire is unaffected.
Double-encoding nested tool args is a recurring habit of OpenAI-wire-trained models, so this is a
class, not a one-off.

The diagnosis surfaced three independent defects — one interop, two forensic/accounting — that
together made the failure both total and invisible:

1. **No stringified-JSON tolerance** in the `EmissionArm::Json` decode
   (`crates/flux-flow/src/compile.rs`, `emit_plan` arm). One `serde_json::from_str` fallback when
   `ast` arrives as a string would have made the whole turn succeed.
2. **The real error is masked.** The decode-failure branch is the only rejection path in the planner
   loop that does not record `last_reject`, so exhausting the step budget reports the uninformative
   variant instead of *"the last plan was rejected: emit_plan: invalid AST JSON …"*. (The
   hallucinated-tool-name branch shares this gap.)
3. **Failed turns lose their token usage.** `compile_turn` accumulates `Usage` across up to
   `max_steps` provider calls but its final `Err` return drops it — s_360 recorded **no**
   `call_usage` event for ~8 × 37k input tokens, so `flux usage` silently undercounts exactly the
   turns that waste the most money.

Session `s_368` (same day, `openrouter/deepseek/deepseek-v4-flash:nitro`, v0.2.14 binary) exposed a
**fourth** defect one layer below all of the above: the OpenAI chat-completions codec parsed
accumulated tool-call arguments with a bare `serde_json::from_str(&args)?`, so args that are not
JSON *at all* — malformed, or truncated mid-emission by the endpoint (a 19KB `emit_plan` blob cut
mid-list at ~2.3k output tokens, far under the planner's 16384 budget) — killed the provider stream
before A-31's reject-feedback loop could engage. Two turns died as
`runtime error: step plan failed: serialization error: …`, one after seven accepted multipass
rounds. The Anthropic-Messages codec had repaired exactly these shapes since the glm/deepseek
incidents (`parse_tool_input`); the hardening had simply never reached the OpenAI wire.

## Approach

Three small, independent stories, all in `crates/flux-flow/src/compile.rs` plus its callers:

- **[A-30](../stories/A-30-stringified-ast-fallback.md) — tolerant stringified-`ast` decode.** In the
  `EmissionArm::Json` arm, when the `ast` value (or the bare `input` fallback) is a JSON **string**,
  parse the string as JSON first and decode `DraftAst` from the result; only if that fails, surface
  the original strict error. Tolerance is *encoding-level only* — the decoded plan still traverses
  the identical downstream gates (hidden-op surfacing, gather enforcement, `validate_plan`,
  analyze/lower), so no validation or safety behavior changes.
- **[A-31](../stories/A-31-planner-reject-surfacing.md) — every repair feedback is a candidate
  `last_reject`.** Record `last_reject` in the decode-`Err` branch and the not-callable
  (hallucinated tool) branch, so the exhausted-budget error always carries the actual last rejection
  the model saw. Pure error-surfacing; no control-flow change.
- **[C-31](../stories/C-31-planner-usage-on-error.md) — usage survives planner failure.** Return the
  accumulated `Usage` alongside the error (shape decided in-story: `(Result<TurnOutput>, Usage)` or
  equivalent) and have the engine's plan step record `call_usage` for failed consultations, so
  `flux usage` counts them.
- **[A-32](../stories/A-32-openai-wire-tool-args-resilience.md) — OpenAI-wire repair + parse-error
  sentinel (added after s_368).** Both OpenAI-wire parse sites (chat streaming + Responses API) run
  the Messages wire's `parse_tool_input` repair (tolerate trailing junk, balance-close truncation);
  when even repair fails, the codec yields the tool_use block with a sentinel input
  (`flux_core::ARGS_PARSE_ERROR_KEY` + `ARGS_RAW_PREFIX_KEY`) instead of a stream error, and the
  planner converts the sentinel into an A-31 rejection so the model re-emits in-turn. The Messages
  wire's `BlockAcc::finish` gets the same sentinel-instead-of-error treatment (now infallible).
  Repair is parse-level only — repaired plans traverse the identical downstream gates; the sentinel
  is checked *before* any field read because a sentinel object would otherwise serde-decode as an
  empty (accepted!) plan.

## Alternatives considered

- **Fix in the provider codec (`flux-providers`/openrouter):** un-stringify nested tool args at the
  wire layer. Rejected — the wire layer has no schema knowledge; only the `emit_plan` consumer knows
  `ast` must be an object. A blanket "parse any string arg as JSON" at the codec would corrupt
  legitimately-string arguments. (This rejection is about *schema-aware* transformation and does not
  contradict A-32: A-32's codec-side repair is pure JSON-syntax recovery — trailing junk, unbalanced
  brackets — with no knowledge of what any field means.)
- **Rely on repair feedback (status quo):** proven non-convergent — qwen re-emits the identical
  string shape for 8 consecutive steps even with the serde error echoed back verbatim.
- **Prompt-side mitigation** ("pass `ast` as an object, not a string"): cheap but speculative, and
  still wastes a repair round per turn on models with the habit; the decode fallback fixes it for
  zero rounds. Can be layered later if other arg-shape mistakes show up.

## Risks & open questions

- Tolerance must not mask garbage: a string that parses to JSON but not to `DraftAst` must keep the
  informative serde error (pinned by test in A-30).
- C-31 touches `compile_turn`'s public signature — mechanical caller updates in the engine's plan
  op, the one-shot `plan()` wrapper, and tests; keep the change type-driven.
- Residual (not seeded): the events store records no planner-internal traffic, so diagnosing this
  class needs temporary instrumentation every time. An env-gated planner trace
  (`FLUX_PLANNER_TRACE=1` → step/stop-reason/tool-name/decode-error to stderr) would make the next
  forensic free. File if it recurs.

## Acceptance / done

Union of the three stories' acceptance: a string-encoded valid plan compiles and executes exactly
like the object form (and a qwen-shaped fixture turn succeeds); an all-steps-rejected turn reports
the last rejection text; a failed planner consultation records its accumulated usage as a
`call_usage` event. Full gate green (build, test, clippy, fmt, codegate).
