# Turn-intent plugin surfacing

**Status:** implemented · **Story:** A-67 · **Owner:** Timo

## Problem

Flux correctly gates grouped operations, but `ToolSpec.group = None` means "core" and therefore
always advertised. Most installed integration plugins ship ungrouped operations. On the tutorial's
tiny two-file task this expanded the model-facing registry to 636 operations and added 27.5k input
tokens compared with the same run under an empty HOME. The plugin catalog was unrelated to the task,
yet dominated the planner request.

Disabling plugins is not an acceptable fix: installed integrations must remain usable, and hidden
operations must not become an authorization bypass. Choosing a faster model is also insufficient:
Gemini 2.5 Flash returned in 5.7s but falsely claimed it wrote the requested file without emitting a
plan, while GPT-5-mini was correct but spent 46.2s compiling the plan.

## Design

### 1. Ungrouped plugin operations get an implicit group

When the CLI loads a plugin, visible operations that are not already owned by a plugin-authored
group are placed in a generated `plugin.<name>` group. The generated group carries a `turn.intent`
predicate for the plugin name. Existing explicit manifest groups win exactly as today because the
implicit group never claims their operations.

This changes advertisement only. The tools stay registered, and execution still traverses policy,
approval, and guarded IO. A model-emitted plan naming a hidden plugin operation is rejected by the
existing A-04 hidden-op gate.

### 2. The current request supplies auditable intent evidence

Before resolving active groups, the engine compares the current user input with the declared
`turn.intent` signals. Matching is case-insensitive and whole-token/phrase bounded: `slack` matches
`"post this in Slack"` and `slack.message.send`, but not `slackware`.

Each match becomes an ordinary `turn.intent` observation carrying `{"signal":"slack"}`. The
existing group resolver consumes it, and `groups.active` records the signal alongside filesystem and
ambient signals. The engine's existing sticky-group union then keeps a surfaced integration visible
for the remainder of that engine session, preserving monotonic catalog growth and prompt caching.

### 3. Explicit control remains available

- A plugin-authored group remains authoritative, including existing force-on groups.
- A workspace group override can force a generated group on when an integration is central to every
  turn in that workspace.
- `FLUX_SURFACE_ALL` continues to expose every registered operation.
- Pre-authored flows remain unrestricted; only model-facing advertisement changes.

## Non-goals and follow-ups

- This story does not wire `--effort`. The CLI currently parses and deliberately discards it; that
  needs one end-to-end setting across planner, finalizer, cognition ops, and sub-agents.
- This story does not redefine op latency. `CliSink` starts its timer before the approval gate, so
  interactive wait is currently displayed as tool execution. Approval-wait telemetry should be
  split from IO/model duration in a separate core story.
- This is not a generic semantic router. Product-name activation is the bounded first step; a
  catalog-search/gather operation may later cover requests that never name an integration.

## Proof

The acceptance comparison uses the same installed plugins, binary, workspace, prompt, and provider
before and after. The key metrics are planner `CallUsage.input_tokens`, wall time, active-group
observations, and actual filesystem result. A named-plugin hermetic test proves discoverability; the
full gate protects correctness and architecture.
