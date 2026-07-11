---
id: C-51
title: Add flux-web to the crates.io publish closure
pillar: Core
status: ready
priority: 11
epic:
design:
note: "prerequisite for downstream consumption of the web capabilities (D-160 web.crawl + D-161 PDF now land in flux-web): add codewandler-flux-web to the crates.io publish closure — a version pin + a publish-script entry; all its flux deps are already published"
---

# Add flux-web to the crates.io publish closure

## Goal
Publish `codewandler-flux-web` with the release so external SDK/plugin consumers can depend on the
web capabilities (`http.request`, `web_fetch` — now with PDF extraction, `web.crawl`, `browser.*`)
from crates.io. Today flux-web is excluded purely by omission and is consumed only in-tree.

## Acceptance
- [ ] Root `Cargo.toml` `flux-web` workspace dep gains a `version` pin at the current workspace
      version (it is path-only today, `Cargo.toml:87`, with a comment documenting the
      exclusion-by-omission convention). `scripts/cut-release.sh` then keeps it in lockstep.
- [ ] `codewandler-flux-web` is added to the `CRATES` array in `scripts/publish-crates-io.sh`,
      positioned after all its deps in the current publish order. Its flux deps — core, runtime,
      spec, system, plugin, markdown, datasource, evidence — are already in the list, so this pulls
      nothing unpublishable into the closure (verified).
- [ ] `crates/flux-sdk/PUBLISHING.md` crate count / list updated to include flux-web (re-check for
      staleness first — a concurrent sdk-surface change may have already refreshed it).
- [ ] Verified with `cargo publish --dry-run` for `codewandler-flux-web` only. **Never publish
      locally** — the real publish runs via CI (`.github/workflows/crates-io.yml`) on the next
      release tag.

## Progress
- 2026-07-11 — READY & PREPARED. The flux-web feature work this unblocks has landed: D-160
  (`web.crawl`) and D-161 (`web_fetch` PDF extraction) are implemented in `crates/flux-web`, so
  publishing flux-web now has concrete external value. The change itself is deliberately **not yet
  applied** — it is release-coupled: the `version` pin must match the version being cut, and the
  actual `cargo publish` only runs via CI on the release tag. Everything needed to execute it in one
  pass is captured in Acceptance below. Flux-web already carries the `codewandler-flux-web` vanity
  name, so only the missing `version` pin (root `Cargo.toml`) and the missing `CRATES` entry
  (`scripts/publish-crates-io.sh`) gate it; its whole flux-dep set is already published (verified).
- Execute at the next release cut, ideally folded into `scripts/cut-release.sh`'s version bump so the
  pin lands in lockstep.

## Notes
- Release-ops, filed standalone (not part of any feature epic) — same convention as C-47.
- Ordering caveat: the in-flight sdk-surface epic (W3) reorders `providers`/`credentials` ahead of
  `sdk` in `scripts/publish-crates-io.sh`; place `codewandler-flux-web` after its deps in whatever
  order the script has when this is implemented.
- Do this AFTER the sdk-surface epic's own publish-order changes land, to avoid editing the same
  `CRATES` array from two sessions at once.
