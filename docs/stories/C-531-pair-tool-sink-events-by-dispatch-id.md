---
id: C-531
title: "Pair tool sink events by dispatch id"
pillar: Core
status: in-progress
priority: 2
epic: tool-output-rendering
design: docs/designs/tool-output-rendering.md
areas: [flux-lang, flux-flow, flux-tui, flux-cli, flux-sdk, flux-server]
note: "C-528's concurrent batches interleave same-name call/result events; the TUI's name-based LIFO match cross-attaches them"
---

# Pair tool sink events by dispatch id

## Goal

A tool result, timing, or progress line can never attach to the wrong transcript card, regardless of
how many same-name calls run concurrently. The sink stream carries the pairing the durable log
already has.

## Acceptance

- [x] `run_call` (`crates/flux-lang/src/runtime.rs`) mints a process-unique dispatch id per call and
      emits it on both the call and its result. A failing-first flux-flow test drives two admitted
      same-name `read` calls through `flush_parallel_native_calls` with a blocking tool releasing
      them out of order, and proves via a collecting sink that each result event carries the id of
      its own call.
- [x] `FlowSink::{tool_call, tool_result}` and `AgentSink::{tool_call, tool_timing, tool_result}`
      carry the id; every overriding implementor is updated (flux-flow SinkBridge/loop_host/whatif,
      flux-cli CliSink/GoalSink/ReviewProgressSink/StreamJsonSink, flux-app, flux-server,
      flux-orchestrate, flux-sdk, flux-tui ChannelSink). Clean cutover — no compat shims.
- [x] The TUI matches `finish_tool`/`time_tool` (and `progress_tool` where an id is present) on the
      id. A failing-first TestBackend test feeds `call(id1, read a)`, `call(id2, read b)`,
      `result(id1)` and asserts card *a* resolves while card *b* stays `◌ running` — today the LIFO
      name scan resolves *b*.
- [x] stream-json emits the id as a new field (additive) with a release-note line.
- [x] The whatif `RerunRecordingSink` FIFO pairing hazard is either fixed by the id or explicitly
      re-scoped in its comments.
- [x] Breaking signature change on published crates is recorded as a workspace MINOR decision.
- [ ] Full repository gate green.

## Progress

- 2026-08-05 — filed from the tool-output review. At filing, C-528's
  `flush_parallel_native_calls` is canonical on origin and arriving via an in-progress merge; the
  cross-attachment becomes the common case the moment it lands.
- 2026-08-05 — implemented on `impl/w0829-c531`. Both failing-first tests were written and RUN
  against the merge base first, and both failed for the real reason:
  - `flux-flow` `staged::tests::concurrent_same_name_results_pair_with_their_own_call` —
    `left: [("a", "b"), ("b", "a")] / right: [("a", "a"), ("b", "b")]`. Two admitted same-name
    `read`s released out of order; arrival order (the only pairing an id-less stream offers)
    attaches each result to the other call.
  - `flux-tui` `tests::tool_result_resolves_its_own_card_not_the_newest_same_name_card` —
    `left: [("alpha.txt", false), ("beta.txt", true)] / right: [("alpha.txt", true),
    ("beta.txt", false)]`, with the frame showing `→ read alpha.txt ⠋ running` next to
    `→ read ▸ beta.txt ✓`.

  `flux_core::DispatchId` (new, `crates/flux-core/src/dispatch.rs`) is a `u64` newtype minted from
  a process-global `AtomicU64` by `DispatchId::next()`, called once per dispatch in `run_call`. It
  rides `FlowSink::{tool_call, tool_result}` and `AgentSink::{tool_call, tool_timing, tool_result}`
  as the first parameter. Clean cutover: no default-parameter bridge, no name fallback anywhere the
  id is available.

  **Version decision — this is a workspace MINOR.** The trait signatures are breaking public API on
  published crates (`codewandler-flux-lang`'s `FlowSink`, `codewandler-flux-flow`'s `AgentSink`,
  and, transitively, every published crate that implements or re-exports them:
  `codewandler-flux-sdk` — whose `AgentEvent::{ToolCall, ToolResult}` also gain a `dispatch` field —
  `codewandler-flux-orchestrate`, `codewandler-flux-server`, `codewandler-flux-app`).
  `codewandler-flux-core` gains `DispatchId` additively. Per the repository's pre-1.0 rule, breaking
  → MINOR: 0.55.0 → 0.56.0. The bump itself is deliberately NOT applied in this story's commit; the
  wave integrator owns it.

  Two acceptance boxes stay unticked on purpose:
  - *stream-json … with a release-note line* — the `dispatch` field IS emitted on both `tool_call`
    and `tool_result`, is asserted end-to-end in `crates/flux-cli/tests/stream_json_smoke.rs`, and
    is documented in `website/docs/agent/cli.md` and `docs/designs/ndjson-agent-protocol.md`. Only
    the CHANGELOG/WHATS-NEW line is missing: those are the wave's shared ledgers, reconciled by the
    integrator, and a child writer must not touch them.
  - *Full repository gate green* — a wave child runs targeted checks only; the gate runs once on the
    combined tree.

  Scoped out with reasons in the code: `progress_tool` still matches by name. A progress line is
  decoded from a `tool.progress` observation raised inside the safety envelope, below the
  interpreter that mints the id, so no id reaches the surface to match on. It stays sound because
  the only producer (the C-158 bash channel) declares `AccessKind::Process`, which
  `native_call_parallel_safe` never admits — two same-name progress-reporting calls are never in
  flight together. `crates/flux-tui/src/lib.rs`'s `progress_tool` doc says so and names the plumbing
  needed if that ever changes.

  Bonus fix riding the same id: `flux-orchestrate`'s live sub-agent reporter paired
  `SpawnActivityEvent::ToolResult` to its call by popping a per-op-name stack (LIFO) — the same
  cross-attachment one layer down, in the fleet pane. It now keys on the dispatch id.

- 2026-08-05 — integrated into wave `flux-wave-20260805-0829`. The release-note line landed in both
  ledgers (`CHANGELOG.md` Fixed, `WHATS-NEW.md` New for the additive `dispatch` field), so that box
  is now ticked; the gate box is ticked from the wave's single full-gate run.

  **Version decision, refined against live tag state.** The MINOR obligation stands, but the target
  is not 0.56.0. At integration, `v0.56.0` already exists as a *local-only* tag on the local-only
  `release-cut-0.56.0` branch, cut from `dc07e60e` — a commit that does not contain this wave. The
  newest tag on `origin` is `v0.55.0`. So this breaking change cannot ship as 0.56.0 unless that
  unpushed cut is re-taken to include the wave; otherwise it forces 0.57.0. No version is bumped
  here — the wave ships the code, and the release owner picks between re-cutting 0.56.0 and cutting
  0.57.0 with full knowledge that the published-crate trait signatures changed.

## Notes

- Evidence: name-based LIFO matching at `crates/flux-tui/src/lib.rs:1857-1948` ("Ops dispatch
  sequentially" premise); concurrency at `crates/flux-flow/src/staged.rs:2890-2949`
  (`native_call_parallel_safe` admits idempotent read-only ops → same-name read/grep/glob batches);
  resume pairs by step id and is already correct (`lib.rs:2968-2971`).
- No provider `tool_use_id` reaches `run_call` (the staged executor wraps each call in a one-call
  AST and drops `call_id`); an `AtomicU64` minted at dispatch is sufficient — call and result
  bracket one await.
- The C-158 bash progress channel never passes `native_call_parallel_safe` (bash is
  `AccessKind::Process`), so name-matching for progress is sound today; plumb the id there only if
  trivial.
- Design: [tool-output-rendering](../designs/tool-output-rendering.md) F-1.
