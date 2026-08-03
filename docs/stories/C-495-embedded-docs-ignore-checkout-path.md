---
id: C-495
title: Embedded docs do not encode the checkout path
pillar: Core
status: done
priority: 0
note: "Docusaurus serialized require.resolve checkout paths into client bundles, so two clean worktrees produced different embedded archives"
---

# Embedded docs do not encode the checkout path

## Goal
The release-matched embedded documentation archive is a deterministic function of committed source,
not of the absolute directory where that source was checked out.

## Acceptance
- [x] A second clean-worktree `scripts/build-embedded-docs.sh --check` reproduces drift against the
      archive generated from the same commit in another directory.
- [x] The Docusaurus configuration supplies path-independent site/plugin identifiers and no built
      JavaScript bundle contains the checkout root.
- [x] Two clean worktrees produce byte-identical archives, and website CI passes.
- [x] The corrected v0.53.0 release is published from the exact green commit.

## Notes
- The differing client chunks contained absolute `sidebars.js`, custom CSS, local plugin, and
  installed search-theme paths serialized from `require.resolve`; page content was identical.
- `scripts/build-embedded-docs.sh` now rejects a generated site containing its checkout root before
  packaging it.
- Docusaurus also generated absolute client-module and Babel-helper requests, and Webpack's default
  numeric module IDs and minifier salted output with those physical filenames. The release bundle
  uses bare helper imports, root-normalized module IDs, and deterministic non-mangled output; zip
  compression keeps the archive overhead bounded.
- At `01ef0d97`, a fresh detached worktree with a fresh `npm ci` reproduced the committed archive
  byte-for-byte (`sha256:3ae672f3d568e43340a25c9ca5747b98ce9d8f44dc4081a333a94cda10d1a8dc`).
- The final release commit `7bd2a8f4` passed website run 30776648680 and exact-SHA candidate run
  30776654391 before `v0.53.0` was tagged. GitHub Release run 30777474649 and crates.io run
  30777474659 then published that exact commit successfully.
