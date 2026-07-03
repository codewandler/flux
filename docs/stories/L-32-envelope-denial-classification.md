---
id: L-32
title: "Classify executor denials structurally, not by prefix-matching tool output"
pillar: Language
status: done
priority: 2
epic: review-hardening
design: docs/designs/review-hardening.md
note: "is_envelope_denial detects a denial by prefix-matching the tool's CONTENT (`` `{op}` denied by ``), so an op that actually ran and relayed that text (e.g. bash surfacing a nested CLI's stderr) is escalated to a fatal, never-retried FlowError::Denied — killing the whole turn instead of feeding a repairable failure back to the loop"
---

# Classify executor denials structurally, not by prefix-matching tool output

## Goal
Stop ordinary tool failures from being misclassified as deliberate authorization refusals and escalated
to a fatal, unrecoverable error. `is_envelope_denial` decides a dispatch was an envelope denial purely by
a content prefix — `content.starts_with(&format!("`{op}` denied by "))` (`crates/flux-flow/src/runtime.rs:77`),
applied at `:86` as `denied: r.is_error && is_envelope_denial(op, &r.content)`. Nothing distinguishes the
envelope's own refusal text from foreign text an op relays: a `bash` (or a nested flux/CLI wrapper) that
exits non-zero with output beginning `` `bash` denied by … `` gets `OpOutcome.denied = true`, and the
interpreter turns that into the fatal, never-retried `FlowError::Denied` — so `retry` / `try` / the agent
loop's self-correction never see it and the whole turn/journey fails on what was merely an ordinary tool
failure. Denials should be flagged structurally by the executor, not inferred from prose.

## Acceptance
- [x] Failing-first test: a dispatch that *ran* and returned an error whose content starts with
      `` `bash` denied by `` is treated as a repairable failure (fed back to the loop / eligible for
      `retry`), **not** a fatal `FlowError::Denied`. Today it aborts the flow.
- [x] Fix: carry a structured denial flag from `Executor::dispatch` (the deny paths already know they
      denied) through `OpOutcome`, instead of prefix-matching `content` in `is_envelope_denial`.
- [x] Genuine envelope denials still surface as `FlowError::Denied`; the existing denial tests pass unchanged.

## Progress
- 2026-07-03 filed — 0.2.11 diff review; grounded correctness (🔴 broad blast radius: affects any turn
  where a tool relays "denied by" text). The executor has no structured denial channel today — every deny
  path returns a plain `ToolResult` and the classification is done downstream by string prefix.
- 2026-07-03 fixed: added `Executor::dispatch_outcome` (`crates/flux-runtime/src/lib.rs`), a sibling of
  `dispatch` that returns the new `DispatchOutcome { result: ToolResult, denied: bool }` — `denied` is
  set structurally at each of the executor's own deny sites (capability scope, policy floor, permission
  rules, approval `Deny`), and left `false` for the unknown-tool path, a pre-tool hook's `Deny`
  (deliberately retryable, per the story's blast-radius note), and any op that ran and merely errored on
  its own. `dispatch` itself is unchanged (a thin wrapper discarding `denied`), so `ToolResult`'s shape
  and every other caller of `Executor::dispatch` across the workspace are untouched — no blast radius
  into flux-tools/flux-capabilities' struct-literal `ToolResult` constructions. `ExecutorHost::dispatch`
  in `crates/flux-flow/src/runtime.rs` now reads `DispatchOutcome::denied` straight through to
  `OpOutcome::denied`; `is_envelope_denial`'s content-prefix match is deleted. New failing-first test
  `op_relaying_denial_shaped_text_is_repairable_not_fatal` (written first, confirmed failing — fatal
  `FlowError::Denied` — against the old prefix-match logic via a temporary revert-and-restore, now
  passing) dispatches a real `bash` op whose command writes `` `bash` denied by nested-cli-policy `` to
  stderr and exits 1: the failure is non-fatal and `retry` re-attempts it 3 times (`sink.calls ==
  ["bash","bash","bash"]`). The existing `policy_denied_op_is_not_retried_inside_loop` (L-21's pinned
  canonical denial shape) passes unchanged — a genuine permission-rule denial still runs exactly once
  and is fatal. Gate: `cargo test -p flux-runtime -p flux-flow` (235 passed across both crates' unit +
  integration suites), `cargo clippy -p
  flux-runtime -p flux-flow --all-targets -- -D warnings` (clean), `cargo fmt -p flux-runtime -p
  flux-flow` (clean).

## Notes
- Evidence: `crates/flux-flow/src/runtime.rs:77-79,86`.
- Related to [L-11](L-11-strict-review-scoped-capabilities.md) (the denial envelope text this key-matches).
  Design: [review-hardening](../designs/review-hardening.md).
