---
id: C-530
title: Stream built-in review flow progress through the shared flow-run sink
pillar: Core
status: done
priority: P1
epic: live-flow-observability
areas: [flux-sdk, flux-app, flux-cli, flux-tui]
note: "flux review must show its context, concurrent reviewers, and aggregation while the immutable built-in flow is running"
---

# Stream built-in review flow progress through the shared flow-run sink

## Goal

Make `flux review` visibly live instead of silent until its final report, without creating a second
review executor. The immutable built-in review protocol continues to run as an authored flow through
the shared flow-run interpreter and `Executor::dispatch`; a shared sink-backed run API exposes the
same operation and correlated child-agent events to a bounded CLI projection.

This story stores the larger architecture as well as the first acceptance-complete slice. The first
slice deliberately uses existing typed `AgentSink` and `SpawnActivity` primitives. It does not infer
private reasoning, expose tool inputs/results, or require new Flux-Lang syntax.

## Acceptance

- [x] Failing-first SDK coverage proves the parse → analyze → seeded execute convenience path can
      forward live events to one shared sink while preserving the ordinary `ExecutionResult`.
- [x] The shared run path installs the sink as the correlated `SpawnActivity` destination, so
      `task(...)` children and nested authored execution can produce live status without a
      review-specific callback or a bypass around `Executor::dispatch`.
- [x] `flux review` runs the existing immutable `STRICT_REVIEW_FLOW_SRC` and embedded, toolless roles
      through that shared sink-backed run path; there is still one review protocol and one execution
      envelope.
- [x] `flux review --progress auto|tree|plain|off` is accepted. `auto` uses a transient tree on an
      interactive stderr and stable append-only summaries otherwise; `off` preserves the old
      silent-until-report behavior.
- [x] Progress is written only to stderr. Markdown and `--format json` final reports remain clean on
      stdout, and `--fail-on` behavior is unchanged.
- [x] The tree shows context gathering, the concurrent reviewer group, each correlated reviewer role
      and closed status, then aggregation. Same-role workers remain distinct by `spawn_id`.
- [x] The customer projection is default-deny: it reads worker identity, role, depth, operation name,
      elapsed/idle age and terminal outcome from `FleetProjection`, but never child thinking, tool
      input, tool result content, or observation payload.
- [x] Non-TTY/plain output contains no cursor controls. The tree renderer does not enter raw mode and
      leaves a complete final frame on success or failure.
- [x] Focused SDK/CLI tests, formatting, relevant clippy, and feasible repository gates are run and
      recorded below.

## Progress

- 2026-08-04 — architecture proposed after observing that `flux review` buffered
  `FlowClient::run_flow` until completion even though `AgentSink`, `SpawnActivity`, and
  `FleetProjection` already carried most of the required live metadata.
- 2026-08-04 — implementation resumed from a workspace containing unrelated in-flight changes;
  C-530 changes must remain narrowly isolated from them.
- 2026-08-04 — completed the sink-backed SDK lifecycle, correlated reviewer projection, stderr
  tree/plain/off renderers, CLI wiring, and periodic idle/stall refresh. The final report and
  severity exit gate remain on their existing path.

## Verification

- `cargo test -p codewandler-flux-sdk run_flow_with_sink_forwards_direct_and_child_activity`
- `cargo test -p flux-cli review_progress_tree_tracks_three_correlated_reviewers_and_phases`
- `cargo check -p flux-cli`
- `cargo fmt --all -- --check`
- `cargo clippy -p flux-cli --all-targets --no-deps -- -D warnings`
- `cargo test -p flux-codegate`

The focused tests cover the shared direct/child event bridge and the bounded three-reviewer phase
projection. The package check, formatting gate, no-dependency CLI clippy gate, and architecture gate
cover the shipped wiring. A broader `cargo clippy -p flux-cli --all-targets -- -D warnings` was also
attempted, but pre-existing changes in `crates/flux-capabilities/src/usage_observatory.rs` fail on
`clippy::misnamed_getters` and `clippy::too_many_arguments`; those unrelated user-owned changes were
left untouched. A provider-backed visual smoke test is not part of the offline gate.

## Architecture

### Built-in flow package

`review` is a host-owned built-in flow, not a project-shadowable stored flow. Its package consists of
an immutable AST/source, declared inputs/output, immutable supporting roles, effects/risk, and
optional host-authored presentation labels. The executable representation is `DraftAst`; embedded
Flux-Lang is one way to produce it, not a requirement for future built-ins.

The bounded slice retains `flux_app::review::STRICT_REVIEW_FLOW_SRC` as that package's source of
truth. A later general registry may add an unshadowable address such as `builtin:review`; it must be
injected by assembly so lower-layer `flux-tools` never depends upward on `flux-app`.

### One flow-run observation seam

Buffered and observed execution are convenience forms over the same lifecycle:

1. parse or receive a `DraftAst`;
2. analyze against the live catalog and seeded input names;
3. execute through the normal flow interpreter and `Executor::dispatch`;
4. forward top-level `AgentSink` events and adapt correlated `SpawnActivity` into that same sink;
5. return the unchanged `ExecutionResult`/outcome.

The model-facing `flow_run`, direct `flux flow run`, built-in review, SDK embeddings, and future TUI
consumers should converge on this seam rather than inventing surface callbacks. Nested `flow_run`
already inherits its enclosing runtime-turn sink; the SDK convenience path must provide the same
child reporter when it is the top-level host.

### Event contract evolution

The first slice consumes existing typed events. A later versioned `FlowRunEvent` envelope may add
`run_id`, monotonic sequence, `node_id`, `parent_id`, timestamps, and explicit run/node/parallel/
branch terminal events. Status remains a closed set (`pending`, `running`, `waiting`, `succeeded`,
`failed`, `cancelled`, `stalled`, `skipped`). Every started entity must eventually terminate on
success, failure, timeout, cancellation, and Ctrl-C.

Interpreter control-flow events must be emitted at the interpreter boundary, not guessed from tool
names. `SpawnActivity` remains authoritative for child agents and is adapted rather than replaced.
No default progress event contains thinking, tool arguments, result content, or arbitrary
observation payload.

### Projection and surfaces

A bounded run-tree projection combines host-authored phases with `FleetProjection` worker rows. The
CLI renders it on stderr as either a transient interactive tree or append-only noninteractive
summaries. Final Markdown/JSON stays on stdout. The TUI should later consume the same projection,
not derive another reviewer state machine.

A future `--events ndjson` mode can serialize versioned structured run events, mutually exclusive
with ordinary final-report stdout. Existing `--format json` remains exactly one final report.

## Notes

- Existing built-in protocol: `crates/flux-app/src/review.rs` and
  `crates/flux-app/assets/flows/strict-review.flux`.
- Existing buffered call site: `crates/flux-cli/src/review.rs::run_review`.
- Shared SDK lifecycle: `crates/flux-sdk/src/flow.rs::FlowClient`.
- Existing child contract and privacy boundary: `flux_runtime::SpawnActivity` and
  `crates/flux-tui/src/fleet.rs::FleetProjection`.
- `AgentSink` remains the live surface extension point; no operation executes outside the normal
  authorization → approval → guarded-IO boundary.
