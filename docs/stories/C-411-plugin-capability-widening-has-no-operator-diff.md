---
id: C-411
title: "A plugin's capability widening is adopted at next load with no operator-visible diff"
pillar: Core
status: in-progress
priority: 8
epic: connector-platform
areas: [flux-plugin]
note: "F5 of the 2026-08-01 security-posture review at 0.47.1. The persisted descriptor records what a plugin asked for; a widened manifest is adopted on the next load without telling the operator what changed"
---

# A widened manifest is adopted silently

## Goal

Make a plugin's capability *widening* something an operator sees and accepts, rather than something
adopted at the next load.

`PluginDescriptor` (`crates/flux-plugin/src/host/loading.rs:757`) persists what the plugin declared.
A plugin that widens its declared capabilities has the new set adopted the next time it loads, with
no diff surfaced to the operator — so the grant an operator reasoned about when they installed it is
not necessarily the grant in force.

This is the same class as C-312's `op_scope_weakenings` and C-311's refresh rule, both of which
refuse a *narrowing* of a stated boundary at refresh. The load path has no equivalent for a
*widening* of what is asked for.

## Acceptance

- [x] **Failing-first**: a test where a plugin loads, then re-loads with a widened capability set,
      asserting the operator is told — failing at the merge base, where it is adopted silently.
- [x] Decide the posture and record it at the definition: surface a diff and require acceptance, or
      refuse the widening until re-installed. "Adopted silently" is the one outcome this story
      forbids.
- [x] Composes with the existing refresh rules rather than fighting them — read
      `op_scope_weakenings` and `PlatformSourcing::strictness` first.
- [x] Full gate green in both workspaces.

## Notes

- Source: `docs/reviews/single/2026-08-01-security-posture-at-0.47.1.md`, F5.
- ⚠ Deny-by-default and manifest-scoping are AGENTS.md safety invariants; this story tightens when a
  scope *changes*, and must not weaken either.

## Progress

- Filed 2026-08-01 from the 0.47.1 security-posture review.
- **Posture chosen: refuse, naming every capability that grew.** Recorded at the definition on
  `CapabilityGrant` (`crates/flux-plugin/src/host/loading.rs`). A diff shown and adopted anyway
  would be a disclosure, not a gate: the load path has no operator attached to accept it (agent
  startup, `flux plugin call`, a server), so the wider grant would already be in force by the time
  anyone read the message.
- **Mechanism.** `PluginDescriptor` gains `capabilities: Option<GrantOfRecord>` — the persisted
  ceiling — plus a non-serialized `origin` (the file it was read from, set by `load_descriptor` /
  `discover`). `load_plugin_tools` measures the fetched declaration against it *before* `make_caps`
  turns the declaration into enforced authority, reusing `refresh::capability_widenings` so the
  load boundary and the refresh boundary answer "is this more authority?" with one function.
- **Bootstrapping.** A descriptor with no record (a fresh install, or one written before this rule)
  is an install, not a widening: the first load writes what the plugin declared back into the
  descriptor. `add_descriptor` then carries that record across every rewrite — `install` onto a new
  version, `pin`, `rollback`, a re-run `add` — so a version switch can never re-grant by accident.
  `flux plugin uninstall` removes the file and with it the record, which is the deliberate re-grant
  the refusal message points at.
- **Composition, not conflict.** The ceiling is asymmetric exactly as `prepare_refresh` is: a
  narrowing loads (the host enforces the narrower set for that session) and does not move the
  record, and returning to the recorded set is not a widening. `refresh.rs`'s module doc now says
  where its "until a restart makes it again" escape was closed.
- **Not weakened:** nothing here grants anything. Deny-by-default and manifest-scoping are
  untouched — this only subtracts, by refusing a load the host would previously have accepted.
