---
id: C-160
title: NDJSON agent protocol — drive and observe a turn over stdio without the SDK
pillar: Core
status: done
epic:
design: docs/designs/ndjson-agent-protocol.md
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
- 2026-07-28: Design doc written first ([docs/designs/ndjson-agent-protocol.md](../designs/ndjson-agent-protocol.md)),
  `design:` frontmatter set, board regenerated. Implementation landed on top of it:
  - New `crates/flux-cli/src/stream_json.rs`: `ProtocolLine` (8 variants — `turn_start`, `plan`,
    `tool_call`, `tool_result`, `approval`, `steered`, `turn_end`, `error`, all `#[non_exhaustive]`,
    each carrying `v: 1`), `StreamJsonSink` (an `AgentSink` impl that writes NDJSON instead of
    rendering for a terminal — same real-time channel `CliSink` uses, no second source of truth),
    the single-turn runner (`run_stream_json`), and the stdin-driven multi-turn runner
    (`run_stream_json_conversation`) with its pure `route_input_line` routing function.
  - `crates/flux-cli/src/args.rs`: `--stream-json` / `--stream-json-input` added directly on
    `Commands::Run` (not the shared `AgentFlags`, to keep them off `tui`/`fork`/`app run --help`).
  - `crates/flux-cli/src/dispatch.rs`: `Commands::Run` routes to the new runners; both flags reject
    `flux run <app.flux>` (program mode) with a clear error; `--stream-json` alone still requires a
    prompt; `--stream-json-input` requires `--yes` (checked in `stream_json.rs`, not just dispatch)
    since v1 has no interactive-approval framing over the input stream.
  - `--stream-json-input` reads NDJSON off stdin on a background task; a `steer: true` line pushes
    onto the engine's A-94 `SteeringQueue` only while a turn is in flight, else falls back to
    queuing as the next ordinary turn (documented + tested both ways).
  - Redaction: the sink clones the SAME `Redactor` the executor dispatches through
    (`Executor::context().redactor` — already a public accessor, the same one `loop_host.rs`'s
    `approve_batch` uses), and redacts every line's full serialized JSON text before writing —
    closing a real gap (`tool_call.input` is never redacted by `Executor::dispatch`, only
    `ToolResult` content/view are).
  - Website docs: new "Machine-readable output" section in `website/docs/agent/cli.md` (line-type
    table + two worked `jq`/heredoc examples, explicitly marked preview/unstable). `npm run build`
    passes (one pre-existing unrelated broken-anchor warning on `flow-client.md`, not touched here).
  - Tests: 5 unit tests in `stream_json.rs` (routing × 3, redaction pass, type+version pinning) +
    5 black-box subprocess tests in the new `crates/flux-cli/tests/stream_json_smoke.rs` (line
    sequence for a `-m mock` run incl. `plan`/`approval`, two-turn stdin conversation, idle-steer
    fallback, `--yes` requirement, secret-in-tool-input redaction). `cargo test -p flux-cli`: 218 +
    all integration suites green. `cargo fmt --all --check` and
    `cargo clippy -p flux-cli --all-targets -- -D warnings` clean.
  - Two corrections made mid-implementation, both recorded in the design doc: (a) the `plan` line's
    real source is `Observation{kind: "action_batch.proposed"}`, not `flow.plan` as first assumed —
    `flow.plan` has renderer-side match arms but **no production emitter** anywhere in
    `flux-flow`/`flux-runtime` today (confirmed by a full-tree grep and empirically against a live
    `-m mock` run); (b) `flow.halt` is similarly renderer-only with no current emitter, so `error` is
    sourced only from `run_turn`'s `Err(_)` in v1.
  - Open nuance for whoever reviews this next: the acceptance's bullet 2 says "projection of the
    existing `RunEvent` stream"; what's actually implemented projects from `AgentSink`/
    `flux_evidence::Observation` — the live per-turn streaming channel `CliSink` already renders
    from — not the persisted `flux-events` `RunEvent` log directly. Every kind used here is also
    flushed to that durable log via the same observation (`flush_observations`), so the two aren't
    in tension, but it's worth a second pair of eyes given how central that acceptance wording is.
  - Status intentionally left `in-progress` and Acceptance boxes left unchecked per explicit
    instruction from the orchestrating session, even though the gate above is green — a deliberate
    checkpoint, not an oversight. `flux-lint`/`--stream-json-thinking` and projecting every other
    `Observation` kind are out of scope for v1 (see the design doc's Non-goals).

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
