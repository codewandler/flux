---
id: C-69
title: Partition plugin guest dependencies from host features
pillar: Core
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: host-kit pulls HTTP, credentials, QuickJS, signing, and archive stacks needed only by the host
---

# Partition plugin guest dependencies from host features

## Goal

Let first-party and third-party guest plugins compile against the framed protocol/manifest SDK
without inheriting flux's host, installer, hooks, transport, and pack-distribution dependency tree.

## Acceptance

- [x] `flux-plugin` exposes a documented guest/protocol feature set that excludes `reqwest`,
      credentials/provider/runtime/system host wiring, QuickJS hooks, signing, and archive/install
      dependencies.
- [x] Host-kit selects `default-features = false` plus only the guest feature set; every nested
      integration plugin builds and tests through that configuration.
- [x] A structural `cargo tree` check proves guest builds do not regain the excluded host-only crates,
      and the story records before/after package count and clean-build time or binary-size evidence.
- [x] Root host loading, guarded callbacks, JS hooks, verified pack install/update, plugin fixtures,
      and CLI/SDK plugin features retain current behavior under explicit host features.
- [x] Wire structs and manifest serialization remain compatible; feature combinations are covered by
      CI/check commands and documented for plugin authors.
- [x] Feature partitioning is preferred; a new protocol crate is introduced only if a written API or
      Cargo-cycle analysis proves features cannot provide the boundary.

## Progress

- 2026-07-14 — Feature-partitioned `flux-plugin`; host-kit now selects only `guest`, while the host
  explicitly retains host/hooks/pack behavior. The structural dependency-tree test excludes the
  host stack and records the normal tree reduction from roughly 237 packages to 80. A clean
  release build of the representative `flux-plugin-alertmanager` binary from an archived
  pre-cutover `HEAD` versus the feature-partitioned tree (empty target directories, same machine
  and warm source cache) fell from 41.106 s / 2,014,936 bytes to 15.098 s / 1,608,624 bytes. The
  guest binary is therefore about 20.2% smaller, with no size regression.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- Current host-kit dependency inspection produced roughly 237 distinct cargo-tree entries and pulled
  host-only `reqwest`, `rquickjs`, and `minisign-verify` through `flux-plugin`.
- Coordinate with C-68 so the final typed guest API is the one feature-partitioned.
