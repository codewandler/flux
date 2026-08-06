---
id: C-626
title: "A worker failure carries its own cause, not a sandbox banner"
pillar: "Core"
status: backlog
epic: fleet-harness-throughput
areas: [flux-cli, flux-orchestrate]
note: "wave-286 died with no recoverable diagnostic; the real error was the last stderr line under two screens of plugin noise; parse_fleet_agent_events context drops the source error. Cost two waves and an unjustified revert."
---

# A worker failure carries its own cause, not a sandbox banner

## Goal

When a worker child dies, the failure event must carry the actual cause. `emitted an invalid
event stream` currently surfaces the child's stderr *banner* — sandbox notes and plugin warnings —
while the real diagnostic is the last line, or is dropped entirely: wave-286 failed with no
recoverable cause at all, and one masked message hid four distinct causes, cost two waves and
triggered an unjustified revert of C-607. `parse_fleet_agent_events`' context drops the source
error today.

## Acceptance

- [ ] The error recorded on agent.turn.failed carries the source parse/validation error, not only captured stderr.
- [ ] Child stderr is reduced to its error-relevant tail (last error line or classified block), with the recurring sandbox/plugin banner blocks stripped or demoted.
- [ ] A wave-286-shaped failure (child exits without terminal event, noisy stderr) yields an event whose error string names the real cause, covered by a test.
