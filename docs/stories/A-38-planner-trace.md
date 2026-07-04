---
id: A-38
title: "FLUX_PLANNER_TRACE=1: env-gated per-step planner trace for zero-instrumentation forensics"
pillar: Agent
status: done
epic: stream-resilience
design: docs/designs/stream-resilience.md
note: "the parse-resilience residual, promoted now that the A-33 backstop makes failures quieter (retries instead of crashes): step / stop_reason / tool names / reject+decode text / dropped-frame diagnostics to stderr"
---

# Env-gated planner trace

## Goal

`FLUX_PLANNER_TRACE=1` emits one stderr line per planner step (step index, stop reason, tool names,
reject/decode text, dropped-frame diagnostics) so the next s_360/s_368-class forensic needs zero
ad-hoc instrumentation. Design: [stream-resilience](../designs/stream-resilience.md); residual
named in [parse-resilience](../designs/parse-resilience.md).

## Acceptance

- [x] Failing-first: `planner_trace_records_step_stop_reason_and_reject_when_enabled` (follow the
      C-19 `fallback_note_sink` test-observable pattern rather than asserting on real stderr).
- [x] Off by default; zero output and zero cost when the env var is unset.
- [x] Full workspace gate green.

## Progress

- 2026-07-04 filed (stream-resilience epic; promoted from the parse-resilience design residual).
- 2026-07-04 implemented, on top of A-33's landed `compile.rs` changes (same file, sequenced
  after). Added an env-gated per-step trace to `compile_turn_inner`'s planner loop:
  - `TraceRecord { step, max_steps, stop_reason, tool_names, reject, dropped_frames,
    diagnostic_detail }` plus `to_line()` — one greppable `key=value` stderr line
    (`flux: planner_trace step=1/8 stop_reason=ToolUse tools=emit_plan reject="..." \
    dropped_frames=- diagnostic=-`).
  - Test-observable sink mirroring C-19's `fallback_note_sink`: since the planner loop is a free
    function (no long-lived provider object to hang a field off), the injection point is a
    `#[cfg(test)]` thread-local (`TRACE_SINK`) instead of a struct field — `install_trace_sink`
    returns an RAII guard a test holds for the call under test; `#[tokio::test]`'s default
    current-thread executor keeps the thread-local valid across every `.await`.
    `planner_trace_enabled()` is the single gate every call site checks first: a sink installed
    on this thread, or the real `FLUX_PLANNER_TRACE=1` env read (cached in a `OnceLock`) in
    production. `emit_trace` delivers to the sink when present, else `eprintln!`.
  - `trace_step(...)` is called once per step at every loop exit (decode-error retry, hidden-ops
    rejection, the text-fallback plan/chat returns, the max_tokens truncation error, the
    dropped-frame/empty-turn path, and the main emit_plan/ask_user processing bottom) — every
    step gets exactly one line regardless of which way it ends. A per-step `last_reject`
    before/after snapshot (`step_reject`) ensures a step that didn't reject anything never
    echoes a stale rejection carried over from an earlier step; `last_reject`'s own semantics and
    A-33's decode-classification are untouched — only read.
  - Zero cost when disabled: every call site is a no-op statement whose body returns immediately
    from `planner_trace_enabled() == false` before any clone/allocation (`step_reject_snapshot`,
    `step_tool_names`, `diag_for_trace` are all built only when tracing is active).
  - Red→green: `planner_trace_records_step_stop_reason_and_reject_when_enabled` was verified
    failing for the right reason by temporarily stubbing `planner_trace_enabled()` to always
    return `false` (`assertion left == right failed: one trace record per planner step: [] left:
    0 right: 2`), then restored — genuine failing-first, not just written green.
  - Gate: `cargo build --workspace` clean; `cargo test --workspace` 88/88 binaries green (0
    failures, flux-flow 208 passed incl. the new test); `cargo clippy --workspace --all-targets
    -- -D warnings` clean; `cargo fmt --check` clean (root + `plugins/`); `cargo test -p
    flux-codegate` 4/4 green.
  - No deviations from the story/design. File-scoped to `crates/flux-flow/src/compile.rs` as
    required; A-33's control flow and `last_reject`/decode-classification semantics untouched.

## Notes

- File: `crates/flux-flow/src/compile.rs`. Sequence after A-33 (same file).
