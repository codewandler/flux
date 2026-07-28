---
id: C-160
title: NDJSON agent protocol — drive and observe a turn over stdio without the SDK
pillar: Core
status: backlog
priority:
epic:
design:
note: "AgentFlags carries no output-format flag (args.rs:77-250) — `--format json` exists only on `flux review` (args.rs:377-380), so a non-Rust caller can only scrape human prose; the event stream (RunEvent) and mid-turn steering (A-94) both already exist internally, this exposes them as one documented line protocol"
---

# NDJSON agent protocol — drive and observe a turn over stdio without the SDK

## Goal
Make flux drivable by *other* programs — CI jobs, editors, other harnesses, the downstream consumer's
service — without linking `flux-sdk`. One JSON object per line out (`--stream-json`), optionally one
JSON object per line in (`--stream-json-input`), including in-band steering of a running turn. This
serves the Agent pillar's platform edge: today the only machine-readable surfaces are the Rust SDK,
the HTTP server, and A2A — all heavier than "pipe a subprocess".

## Acceptance
- [ ] `flux run --stream-json <prompt>` emits one JSON object per line to stdout covering at least:
      turn start/end, plan emitted, per-op dispatch + result (redacted per the evidence rules),
      approval request/decision, usage/cost, and error — each line carrying a `type` discriminator
      and a schema version. Failing-first test asserting the line sequence for a `-m mock` run.
- [ ] The event vocabulary is a **projection of the existing `RunEvent` stream**, not a second
      source of truth — a new event type cannot be added to the protocol without existing upstream.
- [ ] `--stream-json-input` reads the same framing on stdin, allowing a multi-message conversation
      in one process; a line with `{"steer": true}` injects into the **running** turn through the
      A-94 steering path rather than queuing a new turn. Test covers both.
- [ ] Human-readable rendering is suppressed on stdout under `--stream-json` (diagnostics go to
      stderr), so the stream is parseable by `jq` with no filtering.
- [ ] Redaction is enforced on the protocol boundary — a `Redactor`-registered secret never appears
      in any emitted line, pinned by test.
- [ ] The schema is documented on the website with a worked `jq` example.

## Progress
- (not started — filed from the 2026-07-28 Amp feature-mining pass)

## Notes
- Source: [../research/amp.md](../research/amp.md) — Amp's `--stream-json` /
  `--stream-json-input` / `--stream-json-thinking` and its `{"steer": true}` input attribute.
- Evidence the gap is real: `crates/flux-cli/src/args.rs:77-250` (`AgentFlags` — no output-format
  flag), `crates/flux-cli/src/args.rs:377-380` (`--format json` exists, but only on `flux review`).
- The two halves this needs already ship: the durable event stream (`flux-events`) and mid-turn
  steering (**A-94**, merged `795a1b6`).
- Natural sibling of **C-132** (shareable run export) — that renders a *finished* run for humans,
  this streams a *live* run for machines. Both are projections over the same event log, so the
  event→line mapping should be shared or at least consistent.
- Decide deliberately whether the protocol is stable-versioned (a compatibility promise) or
  explicitly unstable in v1; do not leave it ambiguous.
