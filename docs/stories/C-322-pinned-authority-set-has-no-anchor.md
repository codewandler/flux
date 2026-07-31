---
id: C-322
title: "The pinned-authority field set has no anchor, so the next manifest field is adopted silently"
pillar: Core
status: ready
priority: 6
areas: [flux-plugin]
note: "found by C-310's re-review — pin_granted_authority pins four fields and takes the rest via `..fetched`, so a field added to PluginManifest and read by with_manifest is adopted from the REFRESHED manifest with no compile-time or test anchor; that is C-310's round-1 surrender bug reappearing on a new surface"
---

# The pinned-authority field set has no anchor

## Goal

Make it impossible to add an authority-bearing field to `PluginManifest` without deciding whether a
refresh may adopt it.

`pin_granted_authority` (`crates/flux-plugin/src/host/refresh.rs:302-310`) pins four fields —
`capabilities`, `auth`, `endpoints`, `config` — and takes everything else via struct-update
`..fetched`. The set is **complete today**: `SystemHostCaps::with_manifest`
(`crates/flux-plugin/src/host.rs:317-328`) reads exactly five things, and the fifth, `name`, is a
hard refusal at `refresh.rs:180-186` before pinning runs.

But nothing ties the pinned set to what `with_manifest` reads. A future field added to
`PluginManifest` and wired into `with_manifest` is **silently adopted from the refreshed manifest** —
which is exactly C-310's round-1 defect (a refresh moving authority) reappearing on a new surface,
with no compile error and no red test to catch it.

The contrast is in the same file and is the model to copy: `capability_widenings`
(`refresh.rs:360-371`) destructures `PluginCapabilities` with **no** `..` rest pattern, so an
eleventh capability family reds the build in two places — there, and at its test anchor
(`crates/flux-plugin/src/host.rs:2703-2716`).

## Acceptance

- [ ] **An exhaustive anchor.** Adding a field to `PluginManifest` must fail to compile, or red a
      named test, until someone classifies it as pinned or adopted. An exhaustive destructure in
      `pin_granted_authority` is the obvious shape and matches `capability_widenings`; if you choose
      something else, say why it is as strong.
- [ ] **Failing-first**: add a throwaway field to `PluginManifest`, show the anchor reds, remove it.
      Paste both states. A test that merely asserts today's four fields are pinned does not satisfy
      this — it would still pass with a fifth field adopted.
- [ ] The classification of every current field is recorded where the next implementor will read it,
      not only in this story. C-310's re-review already produced that table (`operations`, `version`,
      `groups`, `datasources`, `discovers` adopted, each with the reason it is safe) — carry it into
      the code or the design doc.
- [ ] ⚠ **`discovers` is the live question, and it is not purely hypothetical.** It is adopted today
      and reaches nothing authority-bearing only because `ProviderEntry` snapshots the manifest at
      load and refresh never re-registers it. Decide whether it belongs in the pinned set now, rather
      than leaving it for [C-318](C-318-live-session-registry-refresh.md) to trip over.
- [ ] Full gate green in both workspaces.

## Notes

- Found by [C-310](C-310-plugin-catalog-refresh.md)'s re-reviewer, which verified the current set is
  complete and passed the story on that basis — this is rot prevention, not a live defect. Filed
  separately so that judgement is on the record rather than in an agent's context.
- The severity if it does rot is the same as round 1's: a projection that under-declares authority
  while pinned host capabilities still enforce the original grant, so the two disagree in the
  direction that hides the disagreement.
- Related: `retained_op_weakenings` is O(ops²) over visible ops — fine at the ~50-op scale a real
  plugin has, wrong for a 1000-op connector catalog. Recorded in C-318's notes, not here.
