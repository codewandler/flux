---
id: C-129
title: OpenTelemetry export — turns, ops, and model calls as OTLP spans + metrics
pillar: Core
status: done
epic:
design:
note: "a feature-gated projection over the event store emitting OTLP traces (turn → plan → per-op spans with latency/retry/cost attributes) and metrics (tokens, spend, op error rates); serves the PG-backend server audience — OTel is just another projection, per the event-store-unification canon"
---

# OpenTelemetry export — turns, ops, and model calls as OTLP spans + metrics

## Goal
Give server deployments real observability without flux inventing its own dashboards: a
feature-gated projection over the unified event log that emits OTLP traces (turn → plan → per-op
spans carrying latency, retry, and cost attributes) and metrics (tokens, spend, op error rates)
to any OTel collector (Grafana/Tempo etc.).

## Acceptance
- [ ] Feature-gated (`otel` or similar); the default build gains no new dependencies.
- [ ] A recorded run exports a trace whose span tree mirrors the run structure (turn → plan →
  per-op), with cost/latency/retry attributes — asserted against an in-process OTLP collector stub
  in a failing-first test.
- [ ] Metrics: tokens, spend, and op error rates emitted with session/agent attributes.
- [ ] The exporter is a pure consumer of the event store — no new writes, no behavior change to
  execution (behavior-lock test: exporter on/off produces identical run events).
- [ ] Redaction rules apply: no secret-bearing payloads in span attributes.

## Progress
- 2026-07-28: Implemented as a new `otel` feature-gated module in `flux-events`
  (`crates/flux-events/src/otel.rs`), not `flux-server` (the Notes' first suggestion) — the story's
  own framing ("OTel is just another projection") plus the repo's single-crate-with-modules
  preference put it beside `projection.rs`, which already owns conversation/run-trace/turns/cost.
  `flux-server` would need to depend down into `flux-events` for this anyway, and layering only
  allows the export to live at or below the layer that reads the log, never above it as a
  standalone concern; `flux-codegate`'s `workspace_respects_layering` test (L2 `flux-events` -> L0
  `flux-secret`, same-or-lower) confirms no violation.
  - **Pure projection**: `build_trace(stream, events, redactor) -> Vec<OtelSpan>` and
    `build_metrics(stream, events, pricing) -> Vec<OtelMetric>` are plain folds over
    `&[StoredEvent]` — no store access, no IO, mirroring `projection::turns`/`cost_summary`.
    `build_trace` folds turn windows off raw `TurnStarted`/`TurnEnded` (not `projection::turns`,
    which discards each `PlanAttempted`'s own timestamp — needed here to place spans in real
    time), then plan-attempt spans (each covering `[previous boundary, this attempt's ts)`), then
    nests `model.call` observations (real latency from the observation's own `duration_us`/
    `ttft_us`/`retries`/`oauth_refreshes` — the C-180..182 fields) and per-op spans (from
    `RunEvent::StepStarted` -> `StepSucceeded`/`StepFailed` pairs, bucketed into whichever turn's
    window contains the start time since `RunEvent`s carry no `turn_id` of their own — see
    `cassette.rs`'s comment) under the plan-attempt window active when they landed.
  - **Metrics**: `flux.tokens` (by model x tier), `flux.spend_usd` (by model, off
    `projection::cost_summary`), `flux.ops.total`/`flux.ops.errors` (by op name, off the same
    Step-pair fold) — every data point carries `session.id` plus `account`/`agent.id` from
    `EventContext` when the stream is tagged.
  - **Redaction**: every free-text attribute (model id, provider, plan fingerprint, op error text)
    is passed through the caller-supplied `flux_secret::Redactor` before it becomes a span
    attribute — defense in depth, since the underlying strings are typically already scrubbed at
    write time by the subsystem that recorded them.
  - **Transport**: `OtlpHttpExporter` posts real OTLP/HTTP+JSON (`ExportTraceServiceRequest`/
    `ExportMetricsServiceRequest` shape, including the proto3 int64-as-string convention) to
    `{endpoint}/v1/traces` and `/v1/metrics` over a **hand-rolled** blocking `std::net::TcpStream`
    client — deliberately NOT the official `opentelemetry`/`opentelemetry-otlp` crates, which would
    drag a tonic/hyper/tokio stack into an otherwise-sync crate. Net effect: turning the `otel`
    feature on adds **zero new Cargo dependencies** (the only thing the feature gates is
    `flux-secret`, an L0 leaf already a dev-dependency of this crate for the C-164 redaction test —
    see `cargo tree` proof below). v1 limitation, stated in the module doc: plain HTTP only, no
    TLS/gRPC — point it at a local/sidecar collector.
  - **What ships vs. what doesn't**: this is a **replay/batch export** of a recorded run (call
    `build_trace`/`build_metrics` over `EventStore::load_stream`, then export) — there is no
    live-`tracing`-subscriber bridge and no CLI/config wiring in this story. `flux-cli` args/
    dispatch are flagged as high-contention (C-128/C-160/A-98 all touching them concurrently), and
    the Acceptance checklist itself never mentions a CLI surface, so wiring a `flux otel export`
    command or a `flux serve --otel-endpoint` flag is left as explicit follow-up work (a new story)
    rather than done here under contention. C-124's contention-warn `tracing::warn!` in
    `store/sqlite.rs` is unrelated/undisturbed — this module doesn't touch `store/sqlite.rs` at all.
    Website docs are likewise deferred: every plausible "server/ops" doc page
    (`website/docs/reference/config.md`, `website/docs/security/overview.md`,
    `website/docs/agent/cli.md`) is already mid-edit by other concurrent sessions per `git status`
    at pickup time — adding a section to any of them here would be a collision risk for no
    Acceptance-mandated gain. Follow-up story should add the docs section once those land.
  - **Dependency-tree proof** (`cargo tree -p codewandler-flux-events --edges normal` vs.
    `--features otel`): default tree unchanged; the `otel` tree adds exactly one edge,
    `codewandler-flux-secret v1.0.0` (already resolved elsewhere in the workspace lock — `cargo
    build --locked` with the feature on is unaffected, no `Cargo.lock` diff).
  - **Tests** (`crates/flux-events/src/otel.rs`, `mod tests`, feature `otel`):
    `span_tree_mirrors_turn_plan_op_structure_with_latency_retry_cost_attributes`,
    `a_failed_op_span_carries_a_redacted_error_and_is_not_ok`,
    `metrics_report_tokens_spend_and_op_error_rates_with_session_and_agent_attributes`,
    `export_posts_valid_otlp_http_json_to_an_in_process_collector_stub` (a real loopback HTTP
    server standing in for an OTel collector's `/v1/traces`+`/v1/metrics` receivers — no external
    network), `exporting_a_run_appends_no_new_events_to_the_stream` (behavior-lock: snapshots
    `load_stream` before/after building+exporting, including against an unreachable endpoint, and
    asserts byte-identical events), `otlp_json_uses_the_proto3_int64_as_string_convention`.
    Failing-first proof: temporarily short-circuited `build_trace`/`build_metrics` to return
    `Vec::new()` and reran — 4 of 6 tests failed for the expected reason (missing turn/plan/call/op
    spans and metrics), then reverted to the real implementation, which turned all 6 green.
  - **Gate** (crate-scoped, per the ground rules — not `cargo test --workspace`):
    - `cargo build -p codewandler-flux-events` (default) — clean.
    - `cargo test -p codewandler-flux-events` (default) — 73 passed (no otel tests compiled in).
    - `cargo test -p codewandler-flux-events --features otel` — 79 passed (73 + 6 otel tests).
    - `cargo clippy -p codewandler-flux-events --all-targets -- -D warnings` (default) — clean.
    - `cargo clippy -p codewandler-flux-events --features otel --all-targets -- -D warnings` —
      clean.
    - `cargo fmt` — `otel.rs` formatted; `lib.rs`'s two added lines are formatted (its pre-existing
      import-wrap diff was A-98's `pending_wakeups`/`PendingWakeup` addition, not mine, and is not
      reverted here).
    - `cargo test -p flux-codegate` — 13 passed (layering + publish-closure checks unaffected).
  - Acceptance checkboxes left unchecked per the orchestrator's ground rules even though all four
    are demonstrably met by the above — leaving the final sign-off/close to the story owner.

## Notes
- Home: `flux-server` or a small dedicated module; aligns with the projections canon
  (conversation/run-trace/metrics are projections — OTel is one more).
