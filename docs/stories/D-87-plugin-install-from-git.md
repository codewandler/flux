---
id: D-87
title: Install a plugin from source — `flux plugin install --git <url>`
pillar: Core
status: done
priority: 10
epic: plugin-install-source
note: "third install source next to the signed github pack (D-46..49) and `--dir` local scan: clone a git URL, detect a Rust flux-plugin crate, `cargo build --release`, register the binary. Unblocks GitLab-hosted / third-party plugins that the github+minisign pack channel can't serve."
---

# Install a plugin from source — `flux plugin install --git <url>`

## Goal
Let a user install a plugin straight from a **git URL**: flux clones the repo, detects a Rust
flux-plugin crate, builds it, and registers the binary — so an internal or third-party plugin (e.g.
one hosted on a private GitLab) installs without the signed github pack channel.

## Why
`flux plugin install` today has two sources, neither of which serves an out-of-tree plugin:
1. the **signed pack** (`crates/flux-plugin/src/pack.rs`) — every URL is hardcoded to
   `github.com/<repo>/releases/...` with `DEFAULT_REPO = "codewandler/flux"` and a minisign signature
   verified against a key only flux maintainers hold; a GitLab-hosted, third-party-signed plugin
   cannot ride it;
2. `--dir <path>` — a local scan of already-built binaries (no fetch, no build).

The concrete driver is ai-agent-platform's `flux-plugin-babelforce-manager`, which lives on private
GitLab: there is no remote-install path for it today. A `--git` source — clone + build-from-source,
the `cargo install --git` model — is the missing, source-transparent alternative to a pre-signed pack.

## Acceptance
- [ ] `flux plugin install --git <url> [--tag <t> | --rev <r> | --branch <b>] [--bin <name>]` clones
      the repo at the given ref into a cache (e.g. `~/.flux/plugins/src/<name>/`) and pins the resolved
      commit.
- [ ] Detects a flux plugin crate (a `[[bin]] flux-plugin-*` target / a discoverable plugin manifest)
      and runs `cargo build --release --locked`; a repo that is not a flux plugin fails with a clear,
      actionable error (not a raw cargo dump).
- [ ] Registers the built binary as a descriptor via the existing `--dir` path
      (`flux_plugin::load_descriptor`/`discover`), so `flux plugin call`/`status` and agent discovery
      pick it up; the descriptor records provenance = the git URL + resolved commit (not a signed-pack
      sha256).
- [ ] **Trust gate:** building arbitrary source is code execution — gate it behind explicit consent
      and/or a host allowlist; the resolved commit + a confirm prompt are shown before the build. The
      descriptor is labelled *from-source (unverified)*, distinct from a signed pack and from
      `--dir`'s `UnverifiedLocal`.
- [ ] Idempotent: re-installing the same resolved commit is a no-op; `--force` rebuilds. Optionally
      cache the built binary in the versioned store (`~/.flux/plugins/bin/<name>/<version>/`) the way
      the pack path does.
- [ ] Docs: the three install sources (signed pack · `--dir` · `--git`) and their trust models are
      documented in one place.

## Progress
- Proposed.

## Notes
- Sits beside `pack.rs` (signed github pack, the Plugin-distribution epic D-46..49) and the `--dir`
  local scan as a **third** install source; reuse descriptor registration + the D-48
  `spawn_verified` boundary (its integrity statement becomes "built from commit X" rather than a
  pack hash).
- **Cross-cutting dependency — build-time dep resolution.** The cloned plugin's own dependencies must
  resolve on the installing machine at build time. flux itself is public github (fine), but a private
  SDK dep (e.g. `babelforce-manager-sdk`) needs to be crates.io-public **or** served from a private
  Cargo registry — and GitLab has **no native Cargo registry** (issue gitlab-org/gitlab#33060), so
  that means `gitlab-cargo-shim` / Kellnr / Artifactory, or keeping it a git dep the build env can
  reach. Settle this alongside `--git` install, or it works only where the private deps are reachable.
- Security model mirrors `cargo install --git`: prefer `--locked`, print the resolved commit, require
  a confirm. A future refinement could add per-consumer signing so a from-source install can still be
  attested.
- Consumer: ai-agent-platform `flux-plugin-babelforce-manager` (the babelforce manager control-plane
  plugin) is the first real target.
