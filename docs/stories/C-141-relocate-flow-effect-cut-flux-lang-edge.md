---
id: C-141
title: Relocate FlowEffect and cut the flux-lang edge out of plugin builds
pillar: Core
status: done
priority:
epic: plugin-protocol-decoupling
design: docs/designs/plugin-protocol-decoupling.md
note: every one of the 21 plugins compiles codewandler-flux-lang (a 75-crate subtree) because the guest wire surface names exactly one type from it — FlowEffect at protocol.rs:7/:140
---

# Relocate FlowEffect and cut the flux-lang edge out of plugin builds

## Goal

Remove `flux-lang` from every plugin's dependency graph, so a change to the parser, CST, or
analyzer no longer rebuilds the plugin pack. Independently valuable: it needs no version-line
change and can ship before the rest of the epic.

## Why (evidence)

`crates/flux-plugin/Cargo.toml` depends on `flux-lang` non-optionally. The guest wire surface uses
it for one type: `use flux_lang::ast::FlowEffect` (`protocol.rs:7`), for
`semantic_effects: Vec<FlowEffect>` (`protocol.rs:140`) — documented right there as a *tag
vocabulary*. From `plugins/`, `cargo tree -i codewandler-flux-lang` shows
`flux-lang → flux-plugin → host-kit → all 21 plugins`, and `flux-lang`'s own subtree is 75 unique
crates (flux-core, flux-policy, rowan, futures, sha2, …).

## Acceptance

- [x] `FlowEffect` is defined in a serde-only crate reachable from the guest surface and
      re-exported from `flux_lang::ast`, so existing `flux_lang::ast::FlowEffect` paths compile
      unchanged.
- [x] `flux-plugin`'s dependency on `flux-lang` is gone (or host-feature-gated so a `guest` build
      never pulls it).
- [x] Failing-first test: a check that asserts `flux-lang` is absent from the plugin build graph
      (assert on `cargo metadata` for a plugin package, or a codegate layering rule), red before
      the change and green after.
- [x] `cargo tree -i codewandler-flux-lang` run from `plugins/` returns nothing.
- [x] Full gate green in both workspaces; `scripts/smoke-plugins.sh` still passes.

## Progress
- **Done 2026-07-28.** `FlowEffect` (enum + `tag`/`from_tag`/`lower`) moved to `flux-spec`, which
  gained `flux-policy` (an L0 serde-only leaf) for the `Action` half of `lower`. `flux_lang::ast`
  re-exports the type, so no call site outside the two crates changed and `lower()` stays an
  inherent method. `crates/flux-lang/src/effects.rs` keeps the lowering contract tests.
- Guard: `flux_codegate::tests::plugin_builds_exclude_host_only_crates` resolves the **plugins
  workspace** metadata and fails when any `GUEST_FORBIDDEN` crate appears. Resolving the real
  graph (not reading manifests) is the honest check — `flux-lang` reached the plugins *through*
  `host-kit`, which a manifest read would have missed. Red before, green after.
- Measured on the `gitlab` plugin: **74 → 30 unique crates** in its normal-edge build graph
  (before-number taken from a throwaway worktree at the parent commit). `plugins/Cargo.lock` lost
  366 lines.

## Notes
- Finishes what C-69 (partition plugin guest dependencies) started.
- `flux-spec` now depends on `flux-policy`; both are L0, so the layering lint is unaffected and the
  wire vocabulary stays serde-only.
