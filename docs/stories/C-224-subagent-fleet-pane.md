---
id: C-224
title: The sub-agent fleet pane — render the SpawnActivity stream the TUI currently discards
pillar: Core
status: in-progress
priority: 15
epic: agent-authored-surface
design: docs/designs/agent-authored-surface.md
areas: [flux-tui]
note: "A-79 shipped a correlated, redacted, per-role sub-agent activity stream and flux-tui installs NO sink for it — the only impl in the tree is flux-cli's IgnoredSpawnActivity (main.rs:114), which drops every event; the data is already designed, tested and thrown away"
---

# The sub-agent fleet pane

## Goal
Show what the sub-agents are doing. A-79 already produces the stream — role, child/parent session
correlation, balanced planning state, tool lifecycle with timing, redacted observations
([live-sub-agent-activity.md](../designs/live-sub-agent-activity.md)) — and the TUI throws all of it
away. Install the sink and project it into a pane.

This story is worth doing whether or not the model ever calls `pane.open`: it closes a real gap on
the daily driver, and it proves the pane vocabulary against data that already exists.

## Acceptance
- [x] `flux-tui` installs a `SpawnActivitySink` and projects its events into a host-owned pane. Today
      the tree's only implementation is `IgnoredSpawnActivity` (`crates/flux-cli/src/main.rs:114`);
      the daily-driver surface stops being one of the places this stream dies.
- [x] The pane shows, per live child: role, status, elapsed, and a bounded recent-activity line —
      derived from **fixed or explicitly allowlisted labels**, per the A-79 design's standing
      constraint that tool input and observation data remain an internal sink contract a customer
      surface must default-deny.
- [x] **Child prose and thinking deltas are never rendered.** A-79 excludes them from the sink
      deliberately ("surface privacy boundary"); this story does not reintroduce them by another
      route.
- [x] **Failing-first test:** with two concurrent children of the same role, their events are paired
      to the correct rows by child session id — the correlation A-79 exists to provide, actually used.
- [x] The pane is host-owned: it appears when children are live and retires on its own lifetime rules.
      The model does not open it and cannot close it — but `pane.list` reports it (labelled
      host-owned) so the model does not duplicate it.
- [x] Bounded like any other pane (C-221's caps) and suppressed at narrow widths.

## Progress

Landed on `impl/C-224`. The TUI decodes A-79's stream and renders it in a host-owned pane.

**Where the "sink install" actually is.** flux-tui does not construct a `SpawnActivitySink`, and
cannot usefully: `FlowEngine::run_turn_cancellable` builds a fresh `RuntimeTurnContext` per turn with
`.with_spawn_activity_sink(...)` derived from the caller's own `AgentSink`
(`flux-flow/src/engine.rs:514-532`, `loop_host.rs:218-231`), so anything installed on the executor is
replaced every turn. The engine's `AgentSinkSpawnActivitySink` forwards each event into the parent
sink as a `subagent.activity` observation, and the TUI simply **dropped it**. The fix is the branch
this story's own Notes prescribe, in `ChannelSink::observation` — the exact shape flux-cli uses at
`rendering.rs:838`. So the sink chain now terminates in the TUI instead of dying there; there is no
second sink implementation, deliberately.

**The vocabulary answer (this story's ordering note).** `kind: rows` **cannot** express the fleet
honestly: the operational question is "working or hung?", and answering it needs a live running
indicator and a tint on the stalled row. A payload can have neither by construction — every `rows`
cell renders in one `panel_style()` (C-220 gives the model no style field, on purpose) and the spinner
is Braille, which C-222 reserves precisely so a payload cannot fake a running indicator.

**`PaneData` was nonetheless not widened**, and that is the finding: the fleet's content is
surface-derived from A-79's typed stream and never model-authored, so it does not belong in the
model-facing payload type at all. The widening is a *host-owned pane* instead — `panes::Pane` is now
`Agent(AgentPane) | Fleet`, and `Pane::Fleet` **carries no data**, reading `ChatState::fleet_rows` at
render time. `PaneData` therefore reaches C-223 exactly as C-220 fixed it, and no model-facing
contract changed.

**Trust.** The host pane deliberately does **not** wear ` ◆ agent `: the mark is the user's evidence
that a region was model-authored, and marking harness chrome would make it evidence of nothing. Pinned
both ways by `the_host_fleet_pane_carries_no_agent_mark_and_an_agent_pane_still_does`. Every glyph the
pane draws (`SPINNER`, `◌`, `●`) was already reserved, so C-222's `is_reserved` is unchanged — only
its call-site citations were updated.

Two defects were found during the work and fixed, both now pinned by tests:
- `FleetProjection::rows()` never applied the retention rule (only `apply()` did), so with no further
  events the pane would have hung around forever. `prune(now)` is now public and called per refresh.
- The activity line ended with `stalled`, so a long op name truncated it away
  (`running · read sta…`) — losing the one word that carries the signal under `Theme::MONO`. It now
  leads the line.

## Notes
- Read [live-sub-agent-activity.md](../designs/live-sub-agent-activity.md) before starting. It
  states the contract's deliberate exclusions and the redaction guarantee (the child redactor scrubs
  registered secrets from both JSON **keys and values** before either reaches the reporter), and
  those exclusions are the interesting constraint here.
- The sink's implementations "must not hold a lock across an await, and must not block" — it is
  called from a live child's path. The TUI's existing `ChannelSink` (`controller.rs:169-192`) is the
  right shape to copy: send onto the `UiEvent` channel and return.
- This is the story that tells us whether `kind: rows` is the right primitive. If the fleet needs
  something `rows` cannot express, fix the vocabulary here — before C-223 makes it model-facing and
  therefore harder to change.
