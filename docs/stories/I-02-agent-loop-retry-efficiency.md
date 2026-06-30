---
id: I-02
title: Reduce wasted agent-loop retries
pillar: Improve
status: done
priority:
design:
---

# Reduce wasted agent-loop retries

## Goal
Reduce wasted planning loops when the agent hits deterministic tool failures, especially cargo wrapper
flag duplication and stale edit anchors observed in local session traces.

## Acceptance
- [x] Cargo wrapper ops normalize model-supplied scope/warning flags so wrapper-managed flags are not
      emitted twice.
- [x] The loop retry breaker detects repeated deterministic failure classes even when the full
      transcript is not byte-identical.
- [x] Regression tests cover duplicate cargo flags and stale `edit` anchors.
- [x] Full dev gate is run before the change is called done.

## Progress
- Done on 2026-06-30: implemented cargo arg normalization in `flux-tools` and deterministic failure
  fingerprinting in `flux-flow`'s loop host.

## Notes
- Triggered by analysis of session `s_243`, where duplicate `--workspace` / `--all-targets` cargo
  args and a stale `edit` anchor caused avoidable replanning.
