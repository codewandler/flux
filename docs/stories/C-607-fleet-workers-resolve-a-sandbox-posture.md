---
id: C-607
title: "Fleet workers resolve a sandbox posture"
pillar: "Core"
status: blocked
priority: 7
epic: agent-evidence-scope
areas: [flux-cli]
design: docs/designs/agent-evidence-scope.md
note: "Commands::Fleet sits in the unattended-exempt arm under a comment claiming none of them starts a turn"
---

# Fleet workers resolve a sandbox posture

## Goal

Give a Fleet worker a resolved sandbox posture instead of inheriting whatever the operator's shell
happened to resolve. With no `FLUX_SANDBOX` set the worker resolves `Off` and is spawned unwrapped, so
the workspace path guard is its only confinement of any kind.

## Blocked on

Forcing `SandboxMode::On` at the spawn site (`guarded_agent_run_async`) confines the worker but makes
**every** worker turn fail with `transient-worker: … emitted an invalid event stream` — reproducibly,
where the same story committed successfully without it. The parse error is masked: the context message
prints only stderr, which carries the child's `trusting FLUX_SANDBOXED=1 as an explicit
OUTER-CONFINEMENT assertion` startup warning. Two candidate mechanisms, neither yet established:

1. child startup output reaching **stdout** and corrupting the `--stream-json` NDJSON protocol;
2. the bwrap-wrapped spawn changing how output is delivered to the supervisor's poll loop.

Surfacing the real parse error is the prerequisite — the `with_context` at the `parse_fleet_agent_events`
call site should carry the source error, not just stderr. Reverted for now: a fleet that cannot do any
work is worse than the exposure this closes, and C-606 has removed `bash`/`proc.run` from the default
story capabilities, which was the sharpest edge of it.

## Original goal


## Acceptance

- [ ] Define acceptance.
