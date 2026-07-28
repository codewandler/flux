---
id: C-144
title: Enforce the protocol marker, pin the wire with golden fixtures, guard the version bump
pillar: Core
status: ready
priority: 13
epic: plugin-protocol-decoupling
design: docs/designs/plugin-protocol-decoupling.md
note: PROTOCOL = "flux.plugin.v1" is stamped into every Frame (protocol.rs:10) but no host code ever reads it back — there is no version check anywhere in host.rs, so an incompatible plugin fails as a deserialization error
---

# Enforce the protocol marker, pin the wire with golden fixtures, guard the version bump

## Goal

Make compatibility a checked contract instead of a convention, so dropping the version lockstep
does not drop the guarantee it was standing in for.

## Acceptance

- [ ] The host reads the `protocol` field on frames from a plugin and rejects a mismatch with an
      actionable message naming both sides (e.g. "plugin speaks flux.plugin.v2, this host speaks
      flux.plugin.v1 — upgrade flux or the plugin"), wired into the load path in
      `crates/flux-plugin/src/host.rs`.
- [ ] Failing-first test: a fixture plugin announcing an unknown protocol string is rejected with
      that error rather than a serde failure.
- [ ] Golden wire fixtures: serialized `Frame` and `PluginManifest` JSON checked into the protocol
      crate, asserted to round-trip in both directions — pinning the wire, which Rust signatures
      do not.
- [ ] A drift guard in the style of `shipped_flux_corpus_agreement` / `website_in_sync`: a
      snapshot of the protocol crate's wire surface that fails loudly and must be updated
      deliberately, alongside the protocol version bump.
- [ ] The guard's failure message says what to do: bump the protocol version, or explain why the
      change is wire-compatible.

## Progress
- (not started)

## Notes
- `PROTOCOL` stays a string — `flux.plugin.v1` is already on the wire in shipped binaries, so
  changing its shape would itself be a breaking wire change.
