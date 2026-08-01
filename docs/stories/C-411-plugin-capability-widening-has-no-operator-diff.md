---
id: C-411
title: "A plugin's capability widening is adopted at next load with no operator-visible diff"
pillar: Core
status: ready
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

- [ ] **Failing-first**: a test where a plugin loads, then re-loads with a widened capability set,
      asserting the operator is told — failing at the merge base, where it is adopted silently.
- [ ] Decide the posture and record it at the definition: surface a diff and require acceptance, or
      refuse the widening until re-installed. "Adopted silently" is the one outcome this story
      forbids.
- [ ] Composes with the existing refresh rules rather than fighting them — read
      `op_scope_weakenings` and `PlatformSourcing::strictness` first.
- [ ] Full gate green in both workspaces.

## Notes

- Source: `docs/reviews/single/2026-08-01-security-posture-at-0.47.1.md`, F5.
- ⚠ Deny-by-default and manifest-scoping are AGENTS.md safety invariants; this story tightens when a
  scope *changes*, and must not weaken either.

## Progress

- Filed 2026-08-01 from the 0.47.1 security-posture review.
