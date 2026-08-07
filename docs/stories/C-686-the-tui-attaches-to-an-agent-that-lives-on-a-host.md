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

- [x] `flux tui` can attach to a served agent by URL or by a named binding, authenticated with the
      same bearer credential `flux a2a` uses (credential by reference, never argv); the remote
      session's turns render in the ordinary panes as they stream. (Amended at integration: tool
      calls and results are **not** carried — `flux app run --serve`'s stream sink emits text
      deltas only, so carrying them means new server protocol surface, filed as C-705. The gap is
      stated in the UI itself rather than left as an empty pane, and the decision-point half of
      supervision does work: an approval carries the tool, its subjects and whether it is
      destructive or mutating.)
- [x] Steering works in the direction the protocol supports: messages sent into the live session,
      and cancellation delivered through `tasks/cancel` — with anything unsupported disabled
      visibly rather than silently inert.
- [x] Approvals raised by the remote agent are surfaced and answerable from the attached TUI where
      the served posture allows it, and plainly reported as unavailable where it does not (the
      `--remote-approval` shared-token limit, until C-687).
- [x] Disconnect and reattach are non-destructive: the remote session continues, and reattaching
      replays enough history to make the pane truthful about what happened while detached.
- [x] The docs state precisely which session artifacts live on which machine, so nobody expects
      `flux sessions`/`replay` locally to hold a remote agent's history unless it was exported.

## Design

[docs/designs/tui-attach.md](../designs/tui-attach.md) — the attach seam, the where-does-history-live
decision, and the three protocol gaps found while building against the shipped served surface.

Summary of the two decisions the story turns on:

- **The seam.** A remote turn never becomes a local session event. `flux-tui` declares a
  protocol-free `attach::AttachedAgent` and a deliberately narrow `AttachUpdate` vocabulary
  (text · lifecycle state · artifact · notice — exactly what `message/stream` carries);
  `flux-a2a::attach` implements it over the existing `flux a2a` client; `flux-cli` translates
  between the two. Updates arrive as one new `UiEvent::Attached` arm and reach the *same*
  transcript mutators a local turn uses, through one crossing point that never touches the local
  event store. Tool calls and results are absent because the wire does not carry them, and the
  surface says so rather than leaving the tool pane silently empty.
- **History.** The remote is authoritative. Reattach replays `tasks/get`'s `Task.history`, which is
  projected from the served agent's own store. Attach mode mints no local session, writes nothing
  locally, and leaves `session_id` empty — so an attached conversation cannot appear in
  `flux sessions` or be `flux replay`ed here. Stated per artifact in
  `website/docs/agent/a2a.md#which-session-artifacts-live-on-which-machine`.

## Progress

Implemented on `impl/C-686`.

- `--attach <URL|NAME>` on `flux tui`, mutually exclusive with `--remote`/`--host`/`--fleet` at
  parse time; `--attach-token-env` (default `FLUX_A2A_TOKEN`) and `--attach-context`. A named target
  resolves an `[[endpoint.static]]` binding declaring `protocol = "a2a"` — deliberately not
  `[[host]]`, which is the substrate axis. Credential is always by reference; no flag accepts a
  token value and a `user:pass@` URL is refused.
- Streaming turns, `tasks/cancel`, `tasks/resubscribe` and `tasks/get` history are driven through
  `flux_a2a::attach::AttachedA2aAgent`; capabilities are probed at connect and rendered
  disabled-with-reason when absent.
- Remote approvals are raised in the TUI's existing approval sheet and answered with the request's
  own `fingerprint`; all four `/approvals` postures render as themselves, including the
  shared-operator-token caveat that stands until C-687.

Not done, and filed as candidate work in the design doc: tool activity does not cross the A2A wire
(the served `StreamSink` implements `text_delta` only); a `contextId` has no read-only route to its
task id, so a fresh process replays history only after its first turn; `GET /sessions/{id}` carries
no history.

## Comments

- In progress: dispatched to an implementor in worktree flux-c686 off base a3f62d66. Design-first — the seam question is how a remote agent's turns reach the TUI's view model without masquerading as local session events, and what is authoritative for history on reattach. NOT a release blocker: this is the agent axis, not host/system, so the cut proceeds without it if it is not green in time.
