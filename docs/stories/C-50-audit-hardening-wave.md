---
id: C-50
title: Harden audited safety, correctness, and efficiency seams
pillar: Core
status: done
design: docs/designs/audit-hardening-wave.md
note: concurrent hardening wave from the 2026-07-10 repository audit
---

# Harden audited safety, correctness, and efficiency seams

## Goal
Close the concrete safety, correctness, reliability, and avoidable-cost defects found by the
2026-07-10 repository audit. Preserve flux's guarded-IO architecture while making failures explicit,
bounded, and auditable.

## Acceptance

### Release blockers

- [x] Redirects in native web and plugin HTTP are followed only after every target is re-authorized;
      caller-supplied credentials never cross origins. Failing-first: redirect-to-private and
      cross-origin-secret-header tests.
- [x] Filesystem permission subjects and plugin file scopes cannot be bypassed with an in-workspace
      symlink alias. Failing-first: granted-alias-to-ungranted-target and plugin-scope-escape tests.
- [x] Foreground processes are killed and reaped on cancellation/timeout, and stdout/stderr capture
      is bounded while the child runs. Failing-first: timeout-child-liveness and oversized-output tests.
- [x] Compaction failures preserve a provider-valid session and include attempted compaction usage.
      Failing-first: compaction-error-session-shape and compaction-error-usage tests.
- [x] Cancelling a plugin callback cannot leave the framed protocol deadlocked or desynchronized.
      Failing-first: cancel-during-callback-then-dispatch test.
- [x] A crashed or malformed eval trial is invalid and can never win on zeroed telemetry. Failing-first:
      failed-candidate-does-not-beat-valid-baseline and missing-metrics-invalid tests.
- [x] Flux-Lang timeout/race cannot skip `with_tools` restoration or `scope.finally`; unsafe shared
      bindings are rejected or deterministically reconciled. Failing-first cancellation/analysis tests.

### Correctness and cost controls

- [x] `max_iterations` controls the checked-in agent loop instead of the hard-coded repeat count.
- [x] Planner failure feedback remains visible when the prompt is capped.
- [x] Built-in and plugin/web reads cap allocation while streaming; `file_stat` reports its promised
      metadata without reading file contents twice.
- [x] The improve-tbench flow stops before candidate gates/trials when the worker yields zero valid
      implementations; task extraction accepts fenced/prose-wrapped JSON arrays. This completes I-05's
      queued chain hardening and is reflected in self-improvement status/design.
- [x] Eval trials support configurable bounded concurrency without changing deterministic result order.
- [x] Provider retries honor `Retry-After` and use bounded jitter.
- [x] Nested sub-agent usage is included exactly once in parent usage totals.
- [x] Batch event appends are atomic on supported backends.
- [x] Unknown top-level configuration keys are rejected with an actionable serde error without
      changing documented nested/legacy aliases.
- [x] The declared Rust MSRV is inherited by every workspace crate and enforced in CI at an honestly
      buildable version.

### Verification and explicit dispositions

- [x] Adaptive-thinking activation is decoupled from incidental sink presence, or a focused measurement
      demonstrates that changing it would regress intended behavior and records the disposition here.
- [x] Terminal-bench token/cost telemetry is captured where the adapter exposes it, with unavailable
      fields represented as unavailable rather than zero.
- [x] CHANGELOG (and WHATS-NEW for user-visible changes), design/status documents, and the full workspace
      gate are green (re-verified 2026-07-10 alongside D-88: build, workspace tests, clippy -D warnings,
      fmt --check, codegate all pass; the C-50 CHANGELOG entry shipped with 0.13.3).

## Progress

- [x] 2026-07-10: audit findings reproduced by source tracing and prioritized by safety impact.
- [x] 2026-07-10: pre-change full gate green (`build`, workspace tests, clippy, fmt check, codegate).
- [x] Wave 1: egress, process lifecycle, flow/session, and eval validity hardening.
- [x] Wave 2: path identity, plugin protocol, and Flux-Lang structured cancellation.
- [x] Wave 3: remaining cost, telemetry, compatibility, and atomicity controls.

## Notes

- Architecture and sequencing: [audit-hardening-wave](../designs/audit-hardening-wave.md).
- The external C-47 release-token blocker is not a code defect and remains outside this story.
- No safety gate may be weakened to make an eval or compatibility check pass.
- Adaptive thinking is selected from provider/model capability profiles; sink attachment is not a
  capability signal. Existing provider tests pin the model-gated disposition.
