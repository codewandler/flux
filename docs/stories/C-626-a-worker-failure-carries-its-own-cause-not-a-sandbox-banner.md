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

## A benign case of the same shape — wave-602

The banner problem has a quieter form worth covering by the same mechanism: a sandbox refusal that is
**correct, expected, and harmless** but is emitted as an `error:` line inside an operation that
succeeded.

Every story-worker commit produces this:

    error: Unable to create '/home/timo/projects/flux/.git/packed-refs.lock': Read-only file system

That is git opportunistically packing refs against the shared primary checkout, *after* the commit
object and the branch update have already succeeded. C-609 narrowed the writable set to the git
subpaths a commit actually needs, so this refusal is the fence working as designed.

But it is indistinguishable from a failure at a glance, and it cost real tokens: `wave-602-worker-1`
spent a paragraph of its handoff establishing that its commit had in fact succeeded despite the line,
citing `git_log` and a clean `git_status` as counter-evidence. A worker should not have to prove that
an `error:` in its own tool output was not an error.

So the classification this story asks for has a second job: a line the sandbox emits for a refusal
that did not fail the operation should be demoted, not surfaced as `error:` in the worker's result.
