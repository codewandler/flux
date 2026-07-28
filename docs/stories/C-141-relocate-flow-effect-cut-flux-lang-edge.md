---
id: C-141
title: Relocate FlowEffect and cut the flux-lang edge out of plugin builds
pillar: Core
status: ready
priority: 10
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

- [ ] `FlowEffect` is defined in a serde-only crate reachable from the guest surface and
      re-exported from `flux_lang::ast`, so existing `flux_lang::ast::FlowEffect` paths compile
      unchanged.
- [ ] `flux-plugin`'s dependency on `flux-lang` is gone (or host-feature-gated so a `guest` build
      never pulls it).
- [ ] Failing-first test: a check that asserts `flux-lang` is absent from the plugin build graph
      (assert on `cargo metadata` for a plugin package, or a codegate layering rule), red before
      the change and green after.
- [ ] `cargo tree -i codewandler-flux-lang` run from `plugins/` returns nothing.
- [ ] Full gate green in both workspaces; `scripts/smoke-plugins.sh` still passes.

## Progress
- (not started)

## Notes
- Record the before/after clean `cargo build --release --workspace` wall-clock in `plugins/` — it
  is the headline number for the epic.
- Finishes what C-69 (partition plugin guest dependencies) started.
