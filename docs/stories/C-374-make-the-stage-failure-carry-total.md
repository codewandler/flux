---
id: C-374
title: Make the stage-failure carry total rather than best-effort
pillar: Agent
status: backlog
epic: serving-surface-and-turn-outcome-residuals
design: docs/designs/serving-surface-and-turn-outcome-residuals.md
note: "carry_stage_failure requires both kind and text as strings; a stage that tags differently silently reverts to pre-C-226 laundering, guarded only by a debug_assert that release builds compile out"
---

# Make the stage-failure carry total rather than best-effort

## Goal

Remove the silent-fallthrough that would let the C-226 defect return without a single test failing.

## Acceptance

- [ ] `carry_stage_failure` (`crates/flux-flow/src/loop_host.rs:264-278`) cannot silently drop a
      failure: a tagged error value that does not match the expected shape is a hard error, not a
      no-op. The `debug_assert_eq!` at `engine.rs:1029` is not a release-build guard.
- [ ] First-failure-wins (`loop_host.rs:272`) is either changed or documented in the protocol, so a
      consumer knows later failures in the same turn are not reported.
- [ ] Gather-call failures inside `explore` (`crates/flux-flow/src/staged.rs:1417-1424`) reach the
      outcome, or their exclusion is reasoned — a turn where every gather call failed and the model
      apologised is currently `outcome: ok`, exit 0.
- [ ] End-to-end coverage extends past the intent stage: `crates/flux-cli/tests/stream_json_smoke.rs:149`
      covers `detect_intent` only. Add binary-level cases for exploration, a custom model stage, and
      compaction.
- [ ] Failing-first: a stage that tags a failure with an unexpected shape reds a test today.

## Progress

- 2026-08-01 — filed from validation of OUTCOME-01.

## Notes

- Sub-agent turns get their own engine and their own failure slot, so there is no cross-attribution
  risk here — verified during validation.
