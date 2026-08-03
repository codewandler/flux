---
id: C-471
title: "A file-configured host cannot set the delegated-tree agent ceiling"
pillar: Core
status: done
priority: 6
areas: [flux-config, flux-runtime]
note: "[limits] max_live_agents now reaches the shared runtime census; project config overrides user config, explicit host limits override posture presets, and 0/1 both mean no delegation"
---

# A ceiling only one kind of host can reach

## Goal

Let an operator configuring flux from a file set the delegated-tree agent ceiling, so the bound C-444
introduces is reachable from every host shape rather than only from an SDK embedder.

## The finding

[C-444](C-444-sdk-secure-defaults.md) adds `max_live_agents` and a shared `AgentCensus`
(`crates/flux-runtime/src/limits.rs`), bounding the number of live agents across a **delegated tree** —
the factor that a per-agent execution limit cannot bound, because a tree of `N` agents each permitted `k`
concurrent calls costs `N × k`.

`flux_config::Limits` has **no** `max_live_agents` field. So the ceiling is settable by:

- an SDK embedder, via `ResourceLimits::with_max_live_agents`, or
- anyone selecting `ResourceLimits::autonomous()`, which carries a preset value.

and by **nobody configuring flux from a file** — the CLI, the served path, an operator deployment. Those
hosts get whatever the default is and cannot change it, in either direction: they can neither lower it to
bound a fan-out more tightly, nor raise it if a legitimate workload needs a wider tree and is now refused.

⚠ The second direction is the one that will generate a bug report. A refusal past the ceiling surfaces as
`Error::Other` from `LocalSpawner::spawn` (see C-444's RISKS), so an operator who hits it sees a task
failure with no configuration knob to turn.

## Acceptance

- [x] `max_live_agents` is settable in the config file, wired through `flux_config::Limits` to
      `ResourceLimits`, with a failing-first test proving a file-configured value reaches the census and
      is enforced.
- [x] Precedence is explicit and tested: an SDK-injected value, the config file, the `autonomous()`
      preset, and the default cannot silently disagree about which wins.
- [x] ⚠ The `0` / `1` semantics are documented at the config key, because they are surprising: the census
      counts the **root** agent, so `max_live_agents = 1` means *no delegation at all*, not "one child".
      C-444 has a test naming this; the config surface needs the same warning where an operator will read
      it.
- [x] A config-schema change means the website config reference gains the key, with the fan-out reasoning
      (`N × k`) stated once so an operator can pick a number rather than guess.

## Progress

- 2026-08-03: added `Limits::max_live_agents`, its project-over-user scalar merge, and
  `ResourceLimits::from_config` wiring. A file-source parsing test plus a runtime census test prove
  the configured value admits `limit - 1` children beside the root, refuses the next spawn
  immediately, and releases a place when a child ends.
- 2026-08-03: pinned the remaining precedence boundary at the SDK builder: an explicit host value
  wins unchanged over the autonomous preset rather than being merged with it. Existing C-444 tests
  continue to pin the autonomous preset when limits are omitted and the unbounded supervised
  default.
- 2026-08-03: documented the shared-tree semantics, `0`/`1` behavior, and the composed
  `max_concurrent_tool_calls × max_live_agents` bound in the config reference and release notes.

## Notes

- Disclosed by C-444's implementor as ADJACENT 4, correctly deferred: it is a config-schema change with
  its own surface and its own test obligations.
- ⚠ Consider whether the refusal should be a **typed** backpressure signal rather than
  `Error::Other(...)` before adding a knob that makes people hit it more often. C-444's RISKS flags this
  as "fine at 8, worth a type if anyone lowers it". The census API already returns the typed
  `AgentCensusRefusal`, and the new enforcement test pins it there; translating that through the
  general `flux_core::Error` boundary remains separate error-taxonomy work rather than a prerequisite
  for making the safety ceiling configurable.
- ⚠ The preset's numbers are unmeasured (C-444 ADJACENT 3: 16 calls / 8 agents / 64 MiB / 32 MiB, a
  deliberate first cut). Making the ceiling configurable does not require tuning the default, but do not
  present the default as measured.
- Related: [C-444](C-444-sdk-secure-defaults.md), [C-463](C-463-autonomy-postures.md).
- Filed 2026-08-02 out of C-444's handoff.
