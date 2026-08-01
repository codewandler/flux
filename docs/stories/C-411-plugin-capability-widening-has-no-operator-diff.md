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
- **Mechanism.** `PluginDescriptor` gains `grant: Option<GrantOfRecord>` — the persisted ceiling —
  plus a non-serialized `origin` (the file it was read from, set by `load_descriptor` / `discover`).
  The check runs in `PluginHost::manifest`, reusing `refresh::capability_widenings` for the
  capability half so both boundaries answer "is this more authority?" with one function.
- **The chokepoint is the manifest fetch, not `load_plugin_tools`** (review round 1 caught this).
  `flux plugin call` spawns via `spawn_verified`, fetches the manifest itself, and hands it to
  `SystemHostCaps::with_manifest` — which sets `self.grants = m.capabilities.clone()` — without ever
  calling `load_plugin_tools`. `flux-codegate`'s C-404 census already lists it as a *distinct*
  ingest site. A check in the projection helper left the one command an operator uses to poke a
  plugin by hand adopting a widening silently. `spawn_verified` records `(name, descriptor)` on the
  host, so enforcing at the fetch covers `call`, `status`, agent startup and the SDK at once, and
  `crates/flux-plugin/tests/capability_grant.rs` drives both shapes separately.
- **The record covers all five fields `with_manifest` installs**, not just `capabilities` (review
  round 1). `ensure_http_host_allowed` admits a host via `http_hosts` **or** `endpoint_allows_host`,
  so a plugin already granted `http: true` could widen its reachable hosts by adding an
  `EndpointSpec` with its capability set byte-identical. `GrantOfRecord` therefore records exactly
  the five fields `pin_granted_authority` classifies as PINNED — `capabilities`, `auth`,
  `endpoints`, `config`, `discovers` — destructured exhaustively, so a new manifest field cannot
  become uncovered authority in silence.
- **Only the first fetch on a host is measured.** A second fetch is a catalog refresh, which has
  its own strictly stricter rules; re-adjudicating it there would contradict them, since
  `pin_granted_authority` deliberately accepts-and-ignores a refreshed `endpoints`/`discovers`.
  Two existing C-322 tests caught that directly when the check ran on every fetch.
- **Bootstrapping.** A descriptor with no record (a fresh install, or one written before this rule)
  is an install, not a widening: the first fetch writes what the plugin declared back into the
  descriptor. `add_descriptor` then carries that record across every rewrite — `install` onto a new
  version, `pin`, `rollback`, a re-run `add` — so a version switch can never re-grant by accident.
  A descriptor that *has* a store and cannot be written is a hard error, not a shrug: silently
  continuing would re-enter the recording branch at every later load and adopt a widening with no
  signal at all. `NotRecorded` now means only "no store behind this descriptor" (an embedder built
  it in memory), which is what its doc claims.
- **Re-granting is lossless.** The refusal names the descriptor path and points at removing the
  `[grant]` table first, which keeps `previous`/`version`/`sha256` — so `flux plugin rollback` still
  works offline. Uninstall-then-install is offered as the alternative, not the primary remedy.
- **`flux plugin status` shows the record**, beside the live manifest surface, and says plainly when
  there is not one yet. A plugin whose manifest has outgrown its record reports `unloadable` there,
  with the refusal naming every field that grew — the pre-flight for the point below.
- **Composition, not conflict.** The ceiling is asymmetric exactly as `prepare_refresh` is: a
  narrowing loads (the host enforces the narrower set for that session) and does not move the
  record, and returning to the recorded set is not a widening. `refresh.rs`'s module doc now says
  where its "until a restart makes it again" escape was closed.
- **Not weakened:** nothing here grants anything. Deny-by-default and manifest-scoping are
  untouched — this only subtracts, by refusing a manifest the host would previously have accepted.
- **Known tradeoff, deliberately not fixed here.** A capability-adding upgrade fails *late*:
  `install`/`pin` never fetch a manifest, so the command succeeds and the refusal appears at the
  next load. Making install itself surface and accept the diff means spawning the freshly installed
  binary at install time — a real design decision that belongs in its own story, alongside the
  install-time capability approval prompt F5 also observes is missing. `flux plugin status` is the
  pre-flight in the meantime.
