---
id: A-123
title: The NDJSON/stdio transport — proc://claude and proc://codex join the fleet
pillar: Agent
status: backlog
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
areas: [flux-fleet]
note: "the second transport — it is what proves runtime and transport are genuinely independent axes rather than one axis with a long name"
---

# The NDJSON/stdio transport — proc://claude and proc://codex join the fleet

## Goal
Foreign CLI agents cannot serve A2A over a socket; they speak stdio. Make transport a real second
axis so `proc://claude?proto=ndjson` is a first-class fleet member — dispatchable, observable and
cancellable — rather than a shell-out.

## Acceptance
- [ ] A `Transport` seam in `flux-fleet` with two implementations: `a2a` (the existing
      `A2aClient`) and `ndjson` (framed NDJSON over the child's stdin/stdout).
- [ ] The NDJSON wire is C-160's vocabulary
      ([ndjson-agent-protocol.md](../designs/ndjson-agent-protocol.md)) — turn start/end, plan,
      dispatch/result, usage, error — not a second private protocol.
- [ ] Child-process supervision follows the pattern `flux-plugin`'s host already uses for framed
      stdio children (`crates/flux-plugin/src/host/loading.rs:12`), rather than a new one.
- [ ] Failing-first test: a dispatch over `ndjson` reports the same `AgentStatus` transitions as the
      same dispatch over `a2a` — `Starting → Ready → Busy → Ready` — so the coordinator's monitoring
      code is transport-blind. Offline, against a stub child, no network.
- [ ] Failing-first test: cancellation works over stdio — cancelling an in-flight turn terminates
      the child's work and reports it, rather than leaving a wedged process.
- [ ] Documented plainly: what the `ndjson` transport **cannot** do that `a2a` can (no addressable
      retained task, so no `tasks/get` after the process exits), so the coordinator's sweep does not
      assume parity.

## Progress
- (not started)

## Notes
- Design: [agent-fleet-runtime.md](../designs/agent-fleet-runtime.md) — "Two axes".
- Depends on A-121, A-122. C-160 is in progress; if its line vocabulary is still moving, this story
  pins the subset it uses rather than forking it.
- Adapting each foreign CLI's own flags and session model is the real cost here — keep it to one
  small adapter per kind and resist modelling their differences in the port.
