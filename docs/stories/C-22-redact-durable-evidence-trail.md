---
id: C-22
title: "Redact the durable evidence trail — observations + plan text hit events.db unscrubbed"
pillar: Core
status: done
epic: library-hardening
design: docs/designs/library-hardening.md
note: "the C-13 redactor runs only on the model-facing tool result; the tool_call observation (per-token permission subjects) and the rendered plan_text are built before it and persisted raw to events.db — a Bearer token in a bash/plan arg lands in the clear"
---

# Redact the durable evidence trail — observations + plan text hit events.db unscrubbed

## Goal
Extend C-13 redaction to the durable evidence path. Redaction happens **only** on the tool return value
inside `Executor::dispatch` (`crates/flux-runtime/src/lib.rs:1176`), but the `tool_call` observation — with
raw per-token permission subjects — is built and pushed **before** that (`:1121`), and the accepted-plan
graph `plan_text` is captured raw (`crates/flux-flow/src/loop_host.rs:891`). Both are persisted to
`events.db` by `flush_observations`/`record_plan_attempt` with no redactor in the path (the `EventStore`
holds none). A model-emitted `bash("curl -H 'Authorization: Bearer sk-ant-…'")` persists the secret in the
clear — readable offline via `/evidence`, `flux usage`, the eval harness, and any D-02 tenant export.

## Acceptance
- [ ] Failing-first test: a turn whose observation/plan carries a seeded secret persists a **redacted**
      record to the event store (today the raw secret is retrievable via the projection).
- [ ] Observation `data` and `plan_text` are redacted at the record/flush seam using the same `Redactor` the
      executor uses (seeded from `resolve_secrets` per C-13), before reaching `record_observation` /
      `record_plan_attempt`.
- [ ] Both the tool-call subjects and the rendered plan graph are covered.
- [ ] Design note picks the seam (redact at `evidence.record` vs at flush) and confirms no double-redaction cost.

## Progress
- 2026-07-03 DONE — `flush_observations` redacts each observation's `data`, and `record_plan_attempt` redacts `plan_text`+`error`, through the executor's `Redactor` (new `flux-secret` dep) before persistence. Seam: observations at flush, plan attempts at their record seam (no double-redaction). Test: `redacts_secrets_in_durable_observations_and_plan_text`. Full gate green.

## Notes
- Evidence: `crates/flux-runtime/src/lib.rs:1121` (obs built), `:1176` (result redacted);
  `crates/flux-flow/src/loop_host.rs:891` (plan_text); `crates/flux-flow/src/engine.rs:599` (flush, no redactor).
- Residual of [C-13](C-13-redactor-seeding.md) / [C-14](C-14-durable-evidence-trail.md).
  Design: [library-hardening](../designs/library-hardening.md).
