---
id: D-21
title: Plugin distribution for non-source users (scoping)
pillar: Core
status: done
priority:
epic: plugin-platform-hardening
note: "SCOPED — decision: fetch-on-install from a signed first-party pack channel (plugins-v* GitHub releases, per-plugin per-target archives + minisign-signed plugins-index.json, sha256 before anything executes, versioned store); bundling rejected on coupling not size; follow-ons filed D-46 (release pipeline) -> D-47 (verified remote install) -> D-48 (enforceable pin/rollback) + D-49 (naming/docs pass); design docs/designs/plugin-distribution.md; 4 key-custody/attestation/cadence questions recorded for the owner"
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
- [x] A short design doc under `docs/designs/` that picks one distribution model with rationale, names the CI/
      release changes it implies, and lists the follow-on implementation stories (this story stays scoping-only).
- [x] No code change required to close this story — it produces the plan that unblocks the real work.

## Progress
- **Done (2026-07-02) — scoping delivered.** Design: fetch-on-install from a signed, first-party
  pack channel. Pack releases are their own `plugins-v<version>` GitHub release series (same repo):
  one prebuilt archive per plugin per target + a machine-readable `plugins-index.json` signed with
  minisign. `flux plugin install <name>[@version]` verifies the index signature against a pubkey
  embedded in flux, checks the artifact sha256 BEFORE anything becomes executable, installs into a
  versioned store (`~/.flux/plugins/bin/<name>/<version>/`). Nothing bundles into the core release
  (bundling rejected on coupling, not size — the pack is ~28 MB because the host does all
  privileged IO); no marketplace service (the index is the marketplace seed). Trust design follows
  terraform (sign the aggregate sums) + krew (hash-in-index), avoids helm (executable manifests) and
  gh-extensions (zero verification). Two testable invariants: the index names assets never URLs;
  the index is data never behavior. Plumbing traps found: the dist release.yml tag glob would match
  plugins tags (pack releases are workflow_dispatch-driven), pack releases need --latest=false, and
  `plugin_binaries_in` skips `.exe` (Windows --dir install broken today; fixed in D-47).
  pin/rollback get teeth: descriptors record version+sha256 at install, re-hashed before every
  spawn. Naming trio: the plugin protocol crate / the plugin pack / the plugin CLI.
- **Follow-ons filed:** D-46 → D-47 → {D-48, D-49}, all `epic: plugin-platform-hardening` with the
  design linked.
- **Open questions for the owner:** minisign key custody (repo Actions secret recommended; 2
  accepted pubkeys from day one for rotation?); when to layer sigstore attestations; how long the
  lockstep pack version holds; whether remote install auto-refreshes the generated plugin skill.

## Notes
- Depends on / relates to: D-13 (`flux plugin skill` — already renders installed manifests, a discovery
  primitive), `flux plugin install`/`add`/`pin`/`rollback` (the install + versioning surface that exists), and
  D-19 (uninstall/status — the lifecycle this would feed). The fluxplane-plugin skill's "discoverable
  marketplace" is prior art to evaluate.
- Surfaced while confirming the `plugins/` nested-workspace layout: the pack is only reachable from source today.
