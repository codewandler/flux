---
id: A-43
title: Stream plan skeletons on the OpenAI wire too — surface tool-args deltas as ToolInputDelta
pillar: Agent
status: done
epic: multipass-agent-loop
note: "L-23 residual: the Messages codec forwards input_json_delta as Chunk::ToolInputDelta so plan skeletons stream live; the OpenAI-wire codec (plain openrouter, codex — bedrock is Messages) accumulates tool args internally and never surfaces them — no skeleton there, same as pre-L-23"
---

# Stream plan skeletons on the OpenAI wire

## Goal
L-23 added `Chunk::ToolInputDelta` and wired it in the shared Messages codec; the OpenAI-wire
codec (`crates/flux-providers/src/openai.rs` — plain `openrouter`, `openai`, `codex`) still
accumulates streaming tool arguments internally and emits only the completed block, so plan-mode
skeleton rendering is silent on those providers. Surface the same additive chunk from the
OpenAI-wire delta handling so the L-23 render works wire-independently.

## Acceptance
- [x] The OpenAI-wire codec yields `Chunk::ToolInputDelta` from its tool-args delta handling
      (mirroring `messages/mod.rs`); failing-first codec test.
- [x] A compile-seam test proves skeleton headlines stream on an OpenAI-wire-shaped script.
- [x] Stream-resilience invariants intact (A-33/A-34 tolerance unchanged); gate green.

## Progress
- 2026-07-06 filed — the residual recorded in L-23's Progress.
- 2026-07-06 implemented: `map_chat_stream` (`crates/flux-providers/src/openai.rs`) now yields
  `Chunk::ToolInputDelta` for each `tool_calls[].function.arguments` fragment as it arrives,
  carrying the tool name forward from the slot accumulator (only the first delta of a call index
  reliably carries `function.name` on this wire) — the existing accumulation into `calls` and the
  final completed `Chunk::Block(ToolUse)` are untouched. Failing-first codec test
  `chat_tool_call_args_stream_as_tool_input_delta` (watched fail with 0 deltas emitted before the
  change, then pass). Compile-seam test
  `compile_turn_streams_plan_skeleton_headlines_from_openai_wire_shaped_deltas` in
  `crates/flux-flow/src/compile.rs` feeds a ragged, small-fragment (~3 bytes) `ToolInputDelta`
  sequence — shaped like the OpenAI wire's per-token `arguments` string deltas rather than L-23's
  even fifths — through `compile_turn` and asserts the same two headlines render; the shared
  `tool_call_streamed` test helper (already wire-agnostic) was reused and its doc comment widened
  to name both codecs it now mirrors. All existing A-32/A-33/A-38 malformed/truncated-args
  tolerance tests in `openai.rs` re-run unchanged and stay green. Full gate green (build, test,
  clippy, fmt in both workspaces, layering codegate).

## Notes
- Bedrock speaks Messages and is covered already.
- **Codex/Responses path investigated and scoped OUT.** Codex re-envelopes through
  `map_responses_stream` (`crates/flux-providers/src/openai.rs`), which today has **no per-fragment
  tool-args accumulator at all** — unlike `map_chat_stream`'s `calls` vec (which A-43 piggybacks
  the delta yield onto), the Responses codec only reacts to `response.output_item.done`, the event
  OpenAI fires once a `function_call` item is fully assembled server-side; any
  `response.function_call_arguments.delta` event the real API sends is silently swallowed by the
  codec's catch-all `_ => {}` match arm, both before and after this story. Surfacing streaming
  skeletons there would mean introducing brand-new per-item accumulation state (keyed by
  `item_id`/`output_index`) for an event type this codec doesn't parse today at all — a
  structurally new feature, not the one-line additive yield this story scoped for the chat/Messages
  shape, and it touches the codex real-time WS re-envelope path the WS/HTTP-parity tests pin
  (`crates/flux-providers/src/codex.rs` line ~131). Left out of this story; a follow-up would need
  its own design pass over the accumulator shape and WS-parity risk.
