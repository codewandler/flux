---
id: C-661
title: "TUI /task starts a sub-agent, with @ references for files and agents"
pillar: "Core"
status: ready
priority: 8
areas: [flux-tui]
epic: tui-board-surface
---

# TUI /task starts a sub-agent, with @ references for files and agents

## Goal

The TUI can talk to the coordinator but cannot start a scoped sub-agent from the transcript. Every
delegation therefore goes through the coordinator's own turn, which serialises work that has no
reason to be serial and buries the child's output inside the parent's.

`/task <text>` should start one sub-agent as a first-class card, and `@` should resolve references
inline — a file to attach, an agent to address — so the operator does not hand-assemble paths.

## Acceptance

- [ ] `/task <text>` starts one sub-agent under the attached session and renders it as its own
      card with live status, not as parent transcript text.
- [ ] `@` opens completion over files and known agents; the resolved reference is what the child
      receives, so a typo fails at composition rather than inside the turn.
- [ ] The child inherits an explicit, closed capability set — starting a task from the TUI must not
      widen authority beyond what the surface already documents.
- [ ] Cancelling the card cancels the child within the advertised bound.
- [ ] Failing first, a test covers `@` resolution, including an unresolvable reference.
