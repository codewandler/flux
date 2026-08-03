---
id: C-510
title: "Install and supervise a verified local Exchange release"
pillar: Core
status: ready
priority: 0
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "Milestone 1 runtime prerequisite: pin, verify, install and own the separate local Exchange process without PATH, source-tree or unsigned fallbacks"
---

# Install and supervise a verified local Exchange release

## Goal

Make `flux exchange local start|status|stop` a trustworthy clean-machine lifecycle for the exact
separately released Exchange build this Flux release supports. Flux manages and authenticates the
process, but never becomes an Exchange runtime, bundles its binary, or discovers one from mutable
machine state.

## Acceptance

- [ ] The Flux build pins one exact Exchange release identity and its supported Exchange API and
      `exchange.connection-plan` contract versions. Every `start`, including a start after an
      already-installed cache hit, revalidates that identity and compatibility before executing or
      accepting the process as healthy; an unversioned latest release is never selected.
- [ ] Installation consumes Exchange X-126's signed machine-readable release manifest, verifies the
      manifest with trust material shipped in Flux, verifies the selected platform archive against
      the digest bound by that manifest, and verifies the unpacked executable identity before it can
      run. A missing signature, unknown signer, wrong platform, digest mismatch, archive ambiguity,
      executable mismatch, or incompatible API/plan version refuses without replacing a known-good
      install.
- [ ] The verified executable is installed into a versioned, owner-only Flux-managed cache under a
      per-release lock and becomes visible only by atomic replacement after complete verification.
      Concurrent, interrupted, partial and repeated installs have failing-first tests proving they
      cannot expose a half-installed or permission-widened executable.
- [ ] Production has no unsigned or skip-verification override. An offline import or explicit
      operator-provided artifact is accepted only through the same pinned release identity,
      signature, checksum, platform and compatibility checks; Flux never searches `PATH`, sibling
      source checkouts, Cargo target directories, or an unpinned URL for an Exchange executable.
- [ ] Start records owner-only process metadata that binds a Flux-owned instance to more than a PID
      (including the verified release identity and an unforgeable or start-time process identity),
      starts it loopback-only, and commits ownership only after the expected compatible Exchange is
      healthy. `stop` signals only that owned instance and refuses stale, reused, foreign or
      mismatched process identity instead of killing a name- or PID-matched process.
- [ ] Repeated `start`, `status` and `stop` calls are deterministic and idempotent. Machine-readable
      outcomes distinguish at least not installed, install/verification refused, stopped, starting,
      healthy, incompatible, unhealthy, foreign/stale ownership and stop failure; a second local
      Exchange is never silently created.
- [ ] Tests use a hermetic signed release index/archive fixture and an injected test-only trust seam;
      production trust roots, signature enforcement and compatibility pins cannot be replaced by
      configuration, environment variables, project files or model-controlled input. Tests cover a
      valid install plus tampered manifest, archive and executable cases.
- [ ] Lifecycle state and diagnostics contain no vendor credential, Exchange Service Account token,
      release fetch credential, or secret-shaped command input. The managed binary remains a
      separately downloaded Exchange release artifact, never an official integration plugin or a
      binary copied into Flux's release archives.

## Progress

- (not started)

## Notes

- Cross-repository source: `../flux-roadmap/decisions/0004-flux-manages-a-verified-local-exchange.md`.
- Depends on Exchange X-126 publishing the signed manifest, checksummed platform archives and
  executable identity/compatibility contract. C-509 consumes this lifecycle only after X-126 and
  this story are released.
- Flux and Exchange are separate HTTP processes. Their Rust engine dependency lines do not need to
  resolve together; the pinned release identity and negotiated HTTP contract versions are the
  compatibility boundary.
