---
id: A-122
title: ProcessRuntime — fork an agent through flux-system's guarded spawn
pillar: Agent
status: backlog
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
areas: [flux-fleet, flux-system]
note: "⚠ process-spawn authority — this is bash-class power; it must be opt-in and subject-scoped, never ambient"
---

# ProcessRuntime — fork an agent through flux-system's guarded spawn

## Goal
Let flux start a flux. `proc://flux?program=worker.flux` forks a child `flux app run --serve
127.0.0.1:0`, learns its ephemeral port, and hands back an address the coordinator can talk A2A to —
reusing the guarded-spawn path the `bash` op already goes through rather than opening a second,
weaker one.

## Acceptance
- [ ] `ProcessRuntime` implements `AgentRuntime` and **passes A-121's contract suite unmodified**.
- [ ] The child is spawned through `flux_system`'s guarded command construction
      (`crates/flux-system/src/lib.rs:1938`, async at `:1983`, `sandbox::configure` at
      `sandbox.rs:281`) — the forked agent inherits the parent's sandbox posture.
- [ ] Failing-first test: `start` → `status` reaches `Ready` only after the child's A2A card
      answers, and `endpoint` returns the child's actual bound port (ephemeral `:0`, discovered at
      runtime, never a hardcoded port).
- [ ] Failing-first test: `stop` terminates the child gracefully within the grace period and
      escalates if it does not exit; no orphaned process survives the test.
- [ ] Failing-first test: the op is **not ambiently available** — process spawn is `bash`-class, so
      it is gated behind explicit opt-in exactly as the generic `bash` op is (AGENTS.md is explicit
      that widening reliance on ambient shell power is the wrong direction), and its
      `permission_subject` is the resolved program path, never `*`.
- [ ] The child is started on a **persistent** event store, and the story records why: a
      program-mode server uses `EventStore::in_memory()` (`crates/flux-cli/src/app_cmd.rs:509`), so a
      restarted worker answers `tasks/get` with not-found for everything it ever did.

## Progress
- (not started)

## Notes
- Design: [agent-fleet-runtime.md](../designs/agent-fleet-runtime.md) — "`ProcessRuntime` reuses two
  existing patterns".
- Depends on A-120, A-121.
- No restart policy here by design; the coordinator's sweep re-dispatches work whose runtime reports
  `Exited`.
