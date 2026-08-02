---
id: A-123
title: The NDJSON/stdio transport — proc://claude and proc://codex join the fleet
pillar: Agent
status: ready
priority: 12
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
areas: [flux-runtime, flux-orchestrate]
note: "C-243 shipped the runtime port without an address URI; this story adds the second worker transport against that port and C-160's shipped wire"
---

# The NDJSON/stdio transport — proc://claude and proc://codex join the fleet

## Goal
Foreign CLI agents cannot serve A2A over a socket; they speak stdio. Make transport a real second
axis so a configured Claude/Codex process worker can be dispatchable, observable and cancellable
through C-243's shipped worker lifecycle rather than through an ad-hoc shell-out. Do not revive the
superseded runtime-selecting `proc://` address URI merely to name the adapter.

## Acceptance
- [ ] A transport seam composes with `flux_runtime::AgentRuntime` without leaking stdio handles into
      the L2 trait. It has two implementations: A2A through the existing client and framed NDJSON
      over the child's stdin/stdout.
- [ ] The NDJSON wire is C-160's vocabulary
      ([ndjson-agent-protocol.md](../designs/ndjson-agent-protocol.md)) — turn start/end, plan,
      dispatch/result, usage, error — not a second private protocol.
- [ ] Child-process supervision follows the pattern `flux-plugin`'s host already uses for framed
      stdio children (`crates/flux-plugin/src/host/loading.rs:12`), rather than a new one.
- [ ] Failing-first test: an NDJSON worker reaches `WorkerState::Live`, completes a dispatch with the
      same coordinator-visible result shape as A2A, and stays live for another turn. Offline,
      against a stub child, no network.
- [ ] Failing-first test: cancellation works over stdio — cancelling an in-flight turn terminates
      the child's work and reports it, rather than leaving a wedged process.
- [ ] Documented plainly: what the `ndjson` transport **cannot** do that `a2a` can (no addressable
      retained task, so no `tasks/get` after the process exits), so the coordinator's sweep does not
      assume parity.

## Progress

- 2026-08-02: respecified against C-243's shipped `AgentRuntime` and C-160's shipped NDJSON wire;
  promoted to ready without the superseded `flux-fleet` crate or `AgentAddress` URI.

## Notes
- Design: [agent-fleet-runtime.md](../designs/agent-fleet-runtime.md) — "Two axes".
- C-243 and C-160 are done; this story builds on both.
- Adapting each foreign CLI's own flags and session model is the real cost here — keep it to one
  small adapter per kind and resist modelling their differences in the port.
