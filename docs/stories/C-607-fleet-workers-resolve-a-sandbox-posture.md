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

`Commands::Fleet` sits in the unattended-exempt arm of the sandbox classifier, under a comment
claiming none of its subcommands starts a turn. That is false — `fleet run`, `fleet drive` and the
integrator dispatch all start turns — so fleet-started work resolves a sandbox posture the classifier
believes it will never need.

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

- [ ] Every fleet subcommand that can start a turn resolves an explicit sandbox posture rather than
      inheriting the unattended exemption.
- [ ] The classifier's comment and its arms agree; a subcommand that starts a turn cannot sit in the
      exempt arm.
- [ ] Wave preparation, which legitimately needs to reach across member repositories, keeps the
      access it needs — the posture is applied at the spawn site, not by widening the whole verb.
- [ ] Regression test: a fleet-started turn runs under the declared posture, and adding a
      turn-starting subcommand to the exempt arm fails a test.
