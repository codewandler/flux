---
id: C-322
title: "The pinned-authority field set has no anchor, so the next manifest field is adopted silently"
pillar: Core
status: done
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

- [x] **An exhaustive anchor.** Adding a field to `PluginManifest` must fail to compile, or red a
      named test, until someone classifies it as pinned or adopted. An exhaustive destructure in
      `pin_granted_authority` is the obvious shape and matches `capability_widenings`; if you choose
      something else, say why it is as strong.
- [x] **Failing-first**: add a throwaway field to `PluginManifest`, show the anchor reds, remove it.
      Paste both states. A test that merely asserts today's four fields are pinned does not satisfy
      this — it would still pass with a fifth field adopted.
- [x] The classification of every current field is recorded where the next implementor will read it,
      not only in this story. C-310's re-review already produced that table (`operations`, `version`,
      `groups`, `datasources`, `discovers` adopted, each with the reason it is safe) — carry it into
      the code or the design doc.
- [x] ⚠ **`discovers` is the live question, and it is not purely hypothetical.** It is adopted today
      and reaches nothing authority-bearing only because `ProviderEntry` snapshots the manifest at
      load and refresh never re-registers it. Decide whether it belongs in the pinned set now, rather
      than leaving it for [C-318](C-318-live-session-registry-refresh.md) to trip over.
- [x] Full gate green in both workspaces.

## Notes

- Found by [C-310](C-310-plugin-catalog-refresh.md)'s re-reviewer, which verified the current set is
  complete and passed the story on that basis — this is rot prevention, not a live defect. Filed
  separately so that judgement is on the record rather than in an agent's context.
- The severity if it does rot is the same as round 1's: a projection that under-declares authority
  while pinned host capabilities still enforce the original grant, so the two disagree in the
  direction that hides the disagreement.
- Related: `retained_op_weakenings` is O(ops²) over visible ops — fine at the ~50-op scale a real
  plugin has, wrong for a 1000-op connector catalog. Recorded in C-318's notes, not here.

## Progress

**Done.** `pin_granted_authority` (`crates/flux-plugin/src/host/refresh.rs`) no longer uses
`..fetched`: it destructures `PluginManifest` exhaustively and rebuilds it field by field, so adding
a field reds it twice (`E0027` on the pattern, `E0063` on the initializer). The classification of
every field — pinned vs adopted, each with its reason — lives in that function's doc comment, and is
restated at the test anchor `every_manifest_field_is_classified_pinned_or_adopted`
(`crates/flux-plugin/tests/catalog_refresh.rs`) so the two cannot drift apart without one failing to
compile. This is the same two-site shape `capability_widenings` already carries for
`PluginCapabilities`.

**`discovers` is now PINNED**, not adopted. It is the provider side of the D-26 discovery fan-out:
`PluginRegistry::providers_for` routes a consumer's query for product X to every plugin whose
manifest `discovers` X, and `EndpointBroker` commits whatever that provider answers into the shared
`EndpointRegistry` other components resolve against. Enlisting for a new product across a refresh is
therefore a plugin appointing itself the authority on where that product lives. `plugin list`
discloses `discovers` in the approval surface (`crates/flux-cli/src/plugin_cmd.rs`), so it is
operator-reviewed for a *specific* set — the same standing as `capabilities`. It was inert only
because `ProviderEntry` snapshots the manifest at load and refresh never re-registers it; C-318
removes that accident, and pinning now means the escalation cannot appear when it does. Cost of
being wrong in the strict direction is a restart to add a discovery product — the same trade
`capability_widenings` already documents.

Regression: `a_refresh_cannot_move_the_discoverable_product_set`, driven by a new `drift-discovers`
mode on the `drift_plugin` fixture. It fails at the merge base (the refreshed manifest arrives
carrying `["prometheus", "postgres"]`) and passes with the pin.

`name` was deliberately left **adopted**. A rename is a hard refusal earlier in `prepare_refresh`,
so `fetched.name` is provably already the granted name; pinning it here would silently *accept* a
rename this function was never meant to adjudicate, and would move a decision out of the one place
that handles it. Reasoning is recorded in the doc comment.

`crates/flux-plugin-protocol/**` was **not** touched — the throwaway field existed only for the
failing-first experiment and was removed. `scripts/check-crate-versions.sh` reports
`PASS 0 changed crate(s)`; no version decision is owed.
