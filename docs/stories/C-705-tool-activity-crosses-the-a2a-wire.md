---
id: C-705
title: "Tool activity crosses the A2A wire"
pillar: "Core"
status: backlog
epic: remote-agents
areas: [flux-server]
design: docs/designs/tui-attach.md
note: "C-686's review: flux-server's StreamSink implements text_delta only, so an attached operator sees prose and never learns which files were read or commands ran"
---

# Tool activity crosses the A2A wire

## Goal

An attached TUI shows a served agent's prose and nothing else, because `flux-server`'s `StreamSink`
implements `AgentSink::text_delta` and no other method. So an operator watching a deployed agent
cannot see which files it read or which commands it ran — the retrospective half of supervision is
missing, and C-686 had to state that in the UI rather than deliver it, because fixing it from the
client is impossible: it is new server protocol surface.

The decision-point half already works — an approval carries the tool, its subjects, and whether the
effect is destructive or mutating — so what is missing is the record of what happened, not the
control over what may happen.

## Acceptance

- [ ] Tool calls and their results are carried on `message/stream`, as artifact or data parts, in a
      shape a non-flux A2A client can ignore safely.
- [ ] What crosses is bounded and redacted on the serving side: arguments and results pass the same
      redactor and size ceilings the local transcript applies, and no secret material crosses
      because a tool happened to receive one.
- [ ] An attached client renders them in the ordinary tool pane, and C-686's
      "not carried by this protocol" capability line flips to available rather than being deleted.
- [ ] A served agent that does not emit them is still attachable, and the client still says so.
