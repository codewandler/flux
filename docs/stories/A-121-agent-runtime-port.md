---
id: A-121
title: The AgentRuntime port — start/stop/status/endpoint, with ExternalRuntime and a contract suite
pillar: Agent
status: done
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
areas: [flux-fleet]
note: "SUPERSEDED by C-243: AgentRuntime ships in flux-runtime with WorkerSpec/WorkerStatus; the original flux-fleet shape and state vocabulary were deliberately replaced"
---

# The AgentRuntime port — start/stop/status/endpoint, with ExternalRuntime and a contract suite

## Goal
The lifecycle abstraction itself: one trait every runtime backend implements, plus the baseline
backend for agents flux does **not** own (`a2a://`, `https://` — start and stop are refusals,
`status` is a card fetch). The contract suite written here is what every later backend must pass
unmodified, so it is the story that decides whether the port is right.

## Acceptance
- [ ] `AgentRuntime` in `flux-fleet` with `scheme`, `access`, `start`, `stop`, `status`, `endpoint`,
      and the `AgentStatus` state set (`Starting`, `Ready`, `Busy`, `Unreachable`,
      `Exited { code }`).
- [ ] `ExternalRuntime` implements it: `start`/`stop` return a clear "not owned by this host" error
      rather than pretending; `status` is derived from an agent-card fetch; `endpoint` is the
      address itself.
- [ ] Failing-first test: **`start` returning does not mean ready.** A backend whose agent is not yet
      answering reports `Starting`, and only reports `Ready` once the transport confirms it (for
      `a2a`, the card answered). The test drives a stub that is slow to come up.
- [ ] Failing-first test: `stop` is idempotent — stopping an already-stopped agent succeeds and
      reports `Exited`, it does not error.
- [ ] A **shared contract-test suite** ships as a `tests/<dir>/mod.rs` helper (the same shape A-113
      used for `WorkBoard`), so A-122/A-124/A-125 reuse it with one `mod` line and no `Cargo.toml`
      change.
- [ ] `access()` declares each backend's concrete authority, so a runtime's power is policy-visible
      before it is used.

## Progress

- Closed as superseded by C-243. The port exists, but not in the proposed crate or exact signature:
  `flux-runtime::AgentRuntime` uses an opaque worker id and `WorkerSpec`; `ProcessRuntime` waits for
  readiness before returning, and `ExternalRuntime` reports the liveness it can honestly know. New
  backends must reuse that shipped contract and extract shared contract tests where useful.

## Notes
- Design: [agent-fleet-runtime.md](../designs/agent-fleet-runtime.md) — "The `AgentRuntime` port".
- Depends on A-120.
- Deliberately no restart/keep-alive: see the design's "What this does not attempt".
