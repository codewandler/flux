---
id: D-21
title: Plugin distribution for non-source users (scoping)
pillar: Core
status: backlog
priority:
design:
---

# Plugin distribution for non-source users (scoping)

## Goal
Define how a flux user who did **not** clone the repo obtains the integration plugin pack. Today the only path
is `cd plugins && cargo build --release && flux plugin install`, which requires the source tree and a Rust
toolchain. Anyone who installed flux via `cargo install flux-cli` or a release binary has no way to get the
plugins. This story is a **scoping / epic-seed**: pick the distribution model, not implement it yet.

## Open questions (the scoping work)
- **Model.** Bundled-with-release prebuilt binaries? A `flux plugin install <name>` that downloads a pinned,
  checksummed artifact? A "discoverable marketplace" (the fluxplane-plugin skill references one) with a manifest
  index? Some mix (core pack bundled, long tail fetched)?
- **Trust & supply chain.** How are downloaded plugin binaries verified (signing / checksums / pinning — note
  `flux plugin pin`/`rollback` already exist for versions)? A plugin runs as a subprocess inside the host
  envelope, but the binary itself is still code on the user's machine.
- **Build/release plumbing.** The `plugins/` workspace is deliberately excluded from the root flux gate
  (`Cargo.toml` `exclude = ["plugins"]`) so vendor deps stay out of the main build. How do prebuilt plugin
  binaries get produced and published in CI/`dist` without pulling that weight into the core release?
- **Cross-platform.** Per-target binaries (linux/macos/arch) vs. a source-build fallback.
- **Naming.** Disambiguate in user-facing docs: `crates/flux-plugin` (the protocol *library*) vs.
  `flux-plugin-<name>` (the plugin *binaries*) vs. `flux plugin …` (the *CLI* surface) — the trio is easy to
  conflate.

## Acceptance
- [ ] A short design doc under `docs/designs/` that picks one distribution model with rationale, names the CI/
      release changes it implies, and lists the follow-on implementation stories (this story stays scoping-only).
- [ ] No code change required to close this story — it produces the plan that unblocks the real work.

## Progress
- (not started)

## Notes
- Depends on / relates to: D-13 (`flux plugin skill` — already renders installed manifests, a discovery
  primitive), `flux plugin install`/`add`/`pin`/`rollback` (the install + versioning surface that exists), and
  D-19 (uninstall/status — the lifecycle this would feed). The fluxplane-plugin skill's "discoverable
  marketplace" is prior art to evaluate.
- Surfaced while confirming the `plugins/` nested-workspace layout: the pack is only reachable from source today.
