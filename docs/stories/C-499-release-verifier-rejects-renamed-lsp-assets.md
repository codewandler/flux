---
id: C-499
title: "Release verifier rejects the published LSP package's assets"
area: Release
status: in-progress
priority: 1
areas: [release, supply-chain, lsp]
note: "found by v0.54.0 promotion run 30781537586: cargo-dist names archives from package codewandler-flux-lsp while the closed verifier still admitted only the former flux-lsp package name"
---

# Release verifier rejects the published LSP package's assets

## Goal

Keep the GitHub Release asset set closed and fully attested while accepting the exact LSP archive
names cargo-dist emits after the crate became a publishable `codewandler-flux-lsp` package.

## Acceptance

- [ ] **Failing first:** the staged verifier refuses the complete v0.54.0 candidate set on
      `codewandler-flux-lsp-aarch64-apple-darwin.tar.xz`, matching promotion run 30781537586.
- [ ] The verifier classifies every new LSP archive and installer as executable, downloads and
      attests exactly that set, and continues to reject unknown executables and orphan sidecars.
- [ ] The self-test fixture is the real post-rename candidate inventory and proves all 14 executable
      assets are classified; historical `flux-lsp-*` releases remain verifiable.
- [ ] A patch release promotes the corrected, release-current candidate and both staged and live
      verification complete before it is announced.

## Progress

- 2026-08-03: downloaded candidate run 30780489647 and measured the complete set: five
  `codewandler-flux-lsp` archives, two installers, matching sidecars, the five ordinary `flux-cli`
  archives/installers, source, checksum index and manifest.
- 2026-08-03: the immutable v0.54.0 tag cannot gain the corrected verifier, so the release-fleet
  audit records it as intentionally unshippable and v0.54.1 will be promoted from corrected main.
