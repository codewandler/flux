---
id: L-23
title: Streaming plan-emission render — plan skeleton appears while emit_plan streams
pillar: Language
status: done
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: deliberately sequenced AFTER L-20's emission-arm decision — if native-text wins, streaming render is nearly free (text deltas already stream); if strict JSON wins, it needs incremental JSON parsing in stream_blocks + a plan_delta sink method. Don't build it twice.
---

# Streaming plan-emission render

## Goal
Render node headlines of the plan as `emit_plan` arguments stream in, so a large execution plan is
visible while it is being composed instead of appearing only when complete.

## Acceptance
- [x] Plan skeleton (per-node headline) renders progressively during emission on the winning
      emission arm; final render identical to today's tree.
- [x] No regression to the repair loop (partial/invalid stream still resolves to the same
      rejection/repair behavior).
- [x] Gate green.

## Progress
- **2026-07-06 — implemented.** L-20 kept `json` as the production emission arm
  (`docs/designs/flux-lang-emission-ab.md`), so this is the incremental-JSON path the note
  anticipated. Approach:
  - **Wire-level gap found and closed.** `stream_blocks` (`crates/flux-flow/src/compile.rs`, now
    ~1326) only ever saw a `tool_use` call's arguments once fully assembled
    (`Chunk::Block`) — the shared Messages-protocol codec
    (`crates/flux-providers/src/messages/mod.rs`) already accumulated `input_json_delta` SSE
    events internally but never surfaced them. There was no channel carrying partial tool-call
    JSON into flux-flow at all, so true "renders while composing" needed one: added
    `Chunk::ToolInputDelta { name, partial_json }` to `flux_core::Chunk`
    (`crates/flux-core/src/stream.rs`) — purely additive (verified no exhaustive match over
    `Chunk` exists anywhere in the workspace; every consumer either constructs values or already
    has a wildcard arm) — and emit it from `map_messages_stream_inner`'s existing
    `WireDelta::InputJsonDelta` handling. This crosses outside the originally-scoped file list
    (`flux-core`, `flux-providers`); recorded here as a deliberate, minimal, safety-checked
    deviation — see the report to the calling agent for the full justification. The OpenAI-wire
    codec (`openai.rs`, used by plain `openrouter`/bedrock/codex) was **not** given parity — noted
    as a residual below.
  - **Incremental scan** (`PlanSkeletonScanner` in `compile.rs`): a hand-rolled depth/string
    tracker, not a general JSON parser. Locates the first `"body"` key (necessarily the outermost
    `ast.body`, since a nested composite node's own `body` field can only appear textually after
    the outer array has opened), then watches the running brace/bracket depth return to the
    array's element level to detect each completed top-level statement, slicing its exact JSON
    text out of the accumulated buffer and decoding it as a generic `serde_json::Value` (never the
    typed `Node` — tolerant of anything short of the full analyzer's requirements). Resumable via a
    `scanned` cursor (each delta processed once, not re-scanned), never panics on
    malformed/truncated input (worst case: stops producing headlines), and never touches the real
    `blocks`/`stop_reason`/decode path `stream_blocks` already builds — read-only side channel.
  - **Headline** (`skeleton_headline`/`skeleton_as_call`/`skeleton_arg_hint`): unwraps a `bind`/
    `memo` to the `call` it wraps (the common `$x = op(...)` shape), shows `{index} {op}` plus a
    short scalar-literal hint from the first arg when there is one (e.g. `"2 read
    /app/server.py"`), truncated at 48 chars. A multi-param op's canonical calling convention
    (`write({"path":…,"content":…})` — one bundled object literal, enforced by the analyzer) has no
    scalar first arg, so its headline is just `"1 write"` — still terse and correct, not guessed.
  - **Sink protocol** (`crates/flux-flow/src/agent_sink.rs`): new `fn plan_delta(&mut self,
    _headline: &str) {}` default-no-op method on `AgentSink`, so every existing sink (CLI/TUI/SDK/
    tests) stays source-compatible without changes.
  - **Relaying** (`crates/flux-flow/src/loop_host.rs`, touched ONLY at the `SharedSink`/
    `ChannelSink`/`SinkEvent` forwarding per the file-boundary instruction): `SharedSink::plan_delta`
    locks and forwards; `ChannelSink::plan_delta` sends a new `SinkEvent::PlanDelta(String)`;
    `SinkEvent::apply` relays it. No changes to the ledger/latch machinery L-24 owns in this file.
  - **CLI render** (`crates/flux-cli/src/main.rs`, `CliSink::plan_delta`): updates the live spinner
    label in place (`"planning… · 2 read /app/server.py"`) so the same 80ms ticker that already
    redraws `planning(true)`'s spinner shows the skeleton taking shape; falls back to a plain dim
    line when no spinner is running (styling disabled / non-interactive stderr).
  - **flux-tui**: not implemented (optional per the story's own instruction) — `ChannelSink` in
    `crates/flux-tui/src/lib.rs` inherits the default no-op, so it compiles and behaves exactly as
    before (no regression), just no live skeleton in the TUI yet. **Residual/follow-up**: wire a
    `UiEvent::PlanDelta` alongside `UiEvent::Planning`, rendered in the footer's phase label the
    same way the CLI spinner does.
  - **Residual**: only the shared Messages-protocol codec (Anthropic-direct, `openrouter-anthropic`,
    ollama) emits `Chunk::ToolInputDelta`. The OpenAI-wire codec (`openai.rs` — plain `openrouter`,
    bedrock, codex) does not; a turn on that wire simply gets no live skeleton (today's behavior,
    not a regression) until a follow-up adds parity there.
- **Failing-first proof**: every new/changed test below was run against the pre-change code first
  (no `PlanSkeletonScanner`/`Chunk::ToolInputDelta`/`plan_delta` existed) and failed to compile;
  after implementation all pass. Tests added, all in `crates/flux-flow/src/compile.rs` unless
  noted: `plan_skeleton_scanner_extracts_top_level_headlines_only`,
  `plan_skeleton_scanner_is_resumable_across_arbitrary_chunk_boundaries` (byte-at-a-time feed,
  including a split squarely inside the literal `"body"` key),
  `plan_skeleton_scanner_never_panics_on_malformed_or_truncated_input`,
  `plan_skeleton_scanner_stops_after_the_body_array_closes`,
  `skeleton_arg_hint_truncates_long_string_literals`,
  `compile_turn_streams_plan_skeleton_headlines_as_emit_plan_arguments_arrive` (a scripted
  streamed `emit_plan` call through the real `compile_turn`, asserting the sink's recorded
  headlines AND that the final `render_pretty` tree is byte-identical to the unsplit case),
  `compile_turn_truncation_stops_plan_skeleton_without_crash_and_repair_is_unaffected` (Acceptance
  #2: a `max_tokens` cutoff mid-second-statement still yields exactly the ceiling/split-repair-
  attempt error text and the exact `1 + TRUNCATION_REPAIRS` call count the pre-existing
  `compile_turn_bounds_truncation_repairs_then_errors` test pins, while the sink records one
  headline per attempt for the statement that DID close and none for the one that didn't — no
  crash); `crates/flux-providers/src/messages/mod.rs`'s `parses_a_full_sse_turn` extended to
  collect and assert the new `ToolInputDelta` chunks alongside the existing assertions.
- **Gate (all green)**: `cargo build/test/clippy --all-targets -D warnings` for `flux-flow`,
  `flux-cli`, `flux-tui` (untouched code path verified, no regression), plus (since this story's
  root-cause investigation required crossing into them) `flux-core` and `flux-providers`;
  `cargo test -p flux-codegate`; `cargo fmt --all` (clean, `--check` confirms). Also verified: full
  `cargo build/test/clippy --workspace --all-targets -D warnings` green (742+ tests, 0 failed) —
  extra diligence given `flux_core::Chunk` is depended on widely.

## Notes
- Prereq: L-20 (emission A/B measured — decision: keep `json`,
  `docs/designs/flux-lang-emission-ab.md`). Touchpoints as implemented: `stream_blocks` +
  `PlanSkeletonScanner` (`crates/flux-flow/src/compile.rs`), the `AgentSink::plan_delta` sink
  protocol (`crates/flux-flow/src/agent_sink.rs`), `SharedSink`/`ChannelSink`/`SinkEvent`
  (`crates/flux-flow/src/loop_host.rs`), `CliSink::plan_delta` (`crates/flux-cli/src/main.rs`),
  `Chunk::ToolInputDelta` (`crates/flux-core/src/stream.rs`), the shared Messages codec
  (`crates/flux-providers/src/messages/mod.rs`).
- Follow-ups filed here rather than as new stories (small, clearly scoped): flux-tui skeleton
  rendering; OpenAI-wire-codec (`openai.rs`) `ToolInputDelta` parity for
  `openrouter`/bedrock/codex.
