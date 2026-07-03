---
id: A-32
title: "OpenAI-wire tool-args resilience — malformed/truncated emit_plan JSON must feed back, not kill the turn"
pillar: Agent
status: done
priority: 1
epic: parse-resilience
design: docs/designs/parse-resilience.md
note: "s_368 (deepseek-v4-flash:nitro via plain `openrouter`): two turns died with `runtime error: step plan failed: serialization error: …` — one after SEVEN successful multipass rounds. The Messages wire already repairs exactly this (`parse_tool_input`, names deepseek as offender); the OpenAI wire has a bare `serde_json::from_str(&args)?`"
---

# OpenAI-wire tool-args resilience

## Goal

s_368 (v0.2.14 binary, `openrouter/deepseek/deepseek-v4-flash:nitro`) lost two turns to fatal
serialization errors that the parse-resilience epic (A-30/A-31) was supposed to have tamed:

- seq 18296: `expected `,` or `}` at line 1 column 1456` (malformed args JSON) — killed the turn
  **after 7 accepted multipass rounds** of orient/gather/execute work.
- seq 18418: `EOF while parsing a list at line 1 column 19189` (model stopped mid-args; ~2.3k output
  tokens, well under the 16384 planner budget — endpoint/model truncation, not our cap) — killed the
  turn on its first plan call.

Root cause: the plain `openrouter` provider speaks the OpenAI chat-completions wire, and
`crates/flux-providers/src/openai.rs:373` (chat streaming) + `:899` (responses API) parse the
accumulated tool-call args with a bare `serde_json::from_str(&args)?`. The error escapes the chunk
stream, the provider call fails, and the `plan` step dies — the planner's reject-feedback loop
(A-31's `Err(msg) → last_reject` arm in `compile.rs`) never sees it. The Anthropic-Messages wire
already solved this exact problem: `parse_tool_input` (`messages/mod.rs:215`) reads the first JSON
value tolerating trailing junk, then balance-closes unterminated brackets/strings — its doc comment
literally names *deepseek-v4-flash via OpenRouter* as the trailing-junk offender. The hardening just
never reached the OpenAI wire.

Two layers:

1. **Repair (port the existing helper):** make `parse_tool_input` `pub(crate)` and use it at both
   openai.rs parse sites. This alone repairs the EOF/truncation shape (balance-close) and trailing
   junk. A repaired-but-semantically-broken plan is still gated by the planner's strict decode +
   analyzer — repair is parse-level only.
2. **Feedback (never fatal):** when repair still fails, do NOT `?`-kill the stream. Yield the
   `ToolUse` block with a sentinel input, e.g.
   `{"__args_parse_error": "<serde msg>", "__raw_prefix": "<first ~200 chars>"}`, and teach the
   planner's emit_plan decode to turn the sentinel into a rejection ("your emit_plan arguments were
   not valid JSON (<err>) — re-emit the complete plan as one JSON object"), which A-31 already
   records and feeds back. Direct-tool dispatch rejects the sentinel via normal input validation —
   a repairable `ToolResult::error`, same family as C-32's directory-read guidance.

## Acceptance

- [x] Failing-first codec test (openai.rs): a streamed chat-completions tool call whose accumulated
      args end mid-list (the s_368 EOF shape) yields a usable `ToolUse` block (repaired), not a
      stream error. Mirror for the trailing-junk shape.
      (`chat_tool_args_truncated_mid_list_are_repaired`, `chat_tool_args_with_trailing_junk_are_repaired`)
- [x] Failing-first codec test: args that repair CANNOT fix (e.g. `{"a" "b"}`) still yield a
      `ToolUse` block carrying the parse-error sentinel — the stream completes.
      (`chat_tool_args_unrepairable_yield_parse_error_sentinel`)
- [x] Failing-first planner test (compile.rs): a sentinel-input `emit_plan` becomes a rejection with
      the serde message in the feedback text, and the next attempt can still be accepted — the turn
      survives (today: `step plan failed: serialization error: …`, turn dead).
      (`sentinel_args_are_rejected_and_the_turn_survives`, `exhausted_budget_reports_the_args_parse_error`)
- [x] Responses-API path (openai.rs:899) covered by the same helper.
      (`responses_tool_args_truncated_are_repaired`)
- [x] Messages-wire behavior unchanged (existing `parse_tool_input_handles_model_json_quirks`
      stays green); when its repair fails it gets the same sentinel treatment instead of `?`.
      (`unrepairable_tool_input_becomes_a_sentinel`; `BlockAcc::finish` now infallible)
- [x] Full workspace gate green.

## Progress

- 2026-07-03 filed from s_368 forensics (two fatal `serialization error` turns; live REPL paste
  from the user confirmed the v0.2.14 binary via the C-30 `$? (unpriced)` marker, so this is a
  real post-A-30/A-31 gap, not a stale-binary artifact).
- 2026-07-03 **implemented** — all six new tests written first and confirmed red (each reproducing
  the exact s_368 serde errors), then green. Sentinel keys live in `flux-core`
  (`ARGS_PARSE_ERROR_KEY`/`ARGS_RAW_PREFIX_KEY`) so codec and planner share them without a dep
  edge; `tool_input_or_sentinel` is the one `pub(crate)` entry point in `flux-providers::messages`
  used by both wires. Red-run bonus finding: without the planner gate, a sentinel object DECODED
  as an empty `DraftAst` — i.e. codec-level garbage could have been *accepted* as an empty plan;
  the gate is checked before any field read. Full gate green (build, clippy `-D warnings`, 87
  workspace test suites, fmt both workspaces). Not committed (per repo rule).
