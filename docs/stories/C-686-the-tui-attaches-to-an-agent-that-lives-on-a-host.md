---
id: C-686
title: "The TUI attaches to an agent that lives on a host"
pillar: "Core"
status: in-progress
epic: remote-agents
areas: [flux-tui]
design: docs/designs/operating-a-deployed-host.md
note: "`flux a2a <url>` already gives a REPL against a served agent; the TUI renders from the local event store, so it cannot show a session that lives somewhere else"
---

# The TUI attaches to an agent that lives on a host

## Goal

Two things are already true and they do not meet: `flux app run --serve` serves a whole agent with
`message/stream` and a sessions API, and `flux a2a <url>` chats with it like a local agent — but
that client is a line REPL. The TUI, which is the actual experience (panes, approvals, tool views,
history), renders from the *local* event store, so it cannot show a session that is being produced
on another machine. `flux tui --remote` today means the opposite thing: a local agent whose effects
land remotely. This story adds the missing direction — attach the TUI to an agent that lives on a
host, so a cluster-resident or VM-resident agent is something you watch and steer rather than a
line-buffered conversation.

## Acceptance

- [ ] `flux tui` can attach to a served agent by URL or by a named binding, authenticated with the
      same bearer credential `flux a2a` uses (credential by reference, never argv); the remote
      session's turns, tool calls and results render in the ordinary panes as they stream.
- [ ] Steering works in the direction the protocol supports: messages sent into the live session,
      and cancellation delivered through `tasks/cancel` — with anything unsupported disabled
      visibly rather than silently inert.
- [ ] Approvals raised by the remote agent are surfaced and answerable from the attached TUI where
      the served posture allows it, and plainly reported as unavailable where it does not (the
      `--remote-approval` shared-token limit, until C-687).
- [ ] Disconnect and reattach are non-destructive: the remote session continues, and reattaching
      replays enough history to make the pane truthful about what happened while detached.
- [ ] The docs state precisely which session artifacts live on which machine, so nobody expects
      `flux sessions`/`replay` locally to hold a remote agent's history unless it was exported.


## Comments

- In progress: dispatched to an implementor in worktree flux-c686 off base a3f62d66. Design-first — the seam question is how a remote agent's turns reach the TUI's view model without masquerading as local session events, and what is authoritative for history on reattach. NOT a release blocker: this is the agent axis, not host/system, so the cut proceeds without it if it is not green in time.
