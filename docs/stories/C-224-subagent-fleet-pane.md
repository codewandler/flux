---
id: C-224
title: The sub-agent fleet pane — render the SpawnActivity stream the TUI currently discards
pillar: Core
status: ready
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
- [ ] `flux-tui` installs a `SpawnActivitySink` and projects its events into a host-owned pane. Today
      the tree's only implementation is `IgnoredSpawnActivity` (`crates/flux-cli/src/main.rs:114`);
      the daily-driver surface stops being one of the places this stream dies.
- [ ] The pane shows, per live child: role, status, elapsed, and a bounded recent-activity line —
      derived from **fixed or explicitly allowlisted labels**, per the A-79 design's standing
      constraint that tool input and observation data remain an internal sink contract a customer
      surface must default-deny.
- [ ] **Child prose and thinking deltas are never rendered.** A-79 excludes them from the sink
      deliberately ("surface privacy boundary"); this story does not reintroduce them by another
      route.
- [ ] **Failing-first test:** with two concurrent children of the same role, their events are paired
      to the correct rows by child session id — the correlation A-79 exists to provide, actually used.
- [ ] The pane is host-owned: it appears when children are live and retires on its own lifetime rules.
      The model does not open it and cannot close it — but `pane.list` reports it (labelled
      host-owned) so the model does not duplicate it.
- [ ] Bounded like any other pane (C-221's caps) and suppressed at narrow widths.

## Progress
- (not started — depends on C-221; independent of C-223)

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
