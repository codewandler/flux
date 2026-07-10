---
title: Plugin trust & signing
description: How flux verifies which plugin code runs — a signed index, per-artifact checksums, and a spawn-time hash re-check — and the residual risk it does not remove.
---

# Plugin trust & signing

Plugin trust is the supply-chain side of plugin safety: proving which binary is installed and which
binary is spawned. It is separate from the capability sandbox, which controls what that binary can
reach through flux once it is running.

The everyday install, pin, rollback, and uninstall commands live in
[Using plugins](../plugins/using-plugins.md).

## The trust ladder

The integration plugins ship separately from flux as a **signed pack** (the `plugins-v*` releases).
Every install from the pack is verified end to end and **fails closed — there is no bypass flag**:

1. **Fixed origin.** Every download URL is built only from `(repo, tag, bare-asset-name)` against
   `github.com`. Asset names are validated to be bare (not URL- or path-shaped) and to carry the
   `flux-plugin-` prefix, so a tampered index can never redirect a download somewhere else.
2. **Signed index.** The release's `plugins-index.json` is **minisign-verified** against a public key
   **embedded in the flux binary** before a single byte of it is trusted. There is no
   `--skip-signatures`; a bad signature is a hard error and nothing is fetched.
3. **Checksum before executable.** Each archive's **sha256** is checked against the (now-trusted)
   index entry *before* it is unpacked and made executable. A mismatch means the archive is never
   written to disk.
4. **Install-time recording.** The installed binary's version and sha256 are recorded, along with a
   hash sidecar beside the stored binary, in the versioned store
   `~/.flux/plugins/bin/<name>/<version>/`.

## Enforcement at every spawn

Recording the hash at install time isn't enough — flux **re-hashes the binary every time it spawns
it** and refuses to launch on any drift. A `HashDrift` names the expected and actual hash and stops
the run; it is never a silent fallback. This is also what makes `pin` and `rollback` safe: they are
verified switches over the side-by-side versioned store, and `rollback` refuses an entry that has no
recorded hash rather than blessing unverified bytes.

Locally-built plugins (from a source checkout via `flux plugin install --dir` or `flux plugin add`)
are registered without a recorded hash and run as **unverified local** — labelled as such, so you can
always tell verified pack binaries from your own dev builds.

## What this does — and does not — guarantee

> The trust ladder proves *which* code runs. It does not sandbox what that code does.

- **It does guarantee**: the binary you run is exactly the one the signed release published, byte for
  byte, and hasn't been swapped or corrupted since install.
- **It does not guarantee** the code is harmless. Plugin binaries are **trusted, pinned code — not OS-sandboxed by default.**
  Confinement of what the code can reach through flux is the
  [capability sandbox](./plugin-sandbox.md)'s job (cleared environment, deny-by-default manifest,
  host-does-all-IO); opt-in [OS process sandboxing](./os-sandbox.md) additionally confines what the
  raw binary's syscalls can reach on disk and network. Review installed plugins the way you review
  dependencies.
- **Residual risk, stated honestly**: a compromised repository or CI pipeline could sign a malicious
  artifact, and the signature check would pass. The signing key lives only in a CI secret, never in
  the repo; stronger provenance (build attestation) is a named future hardening, not yet in place.

The signed pack, the sha256 pinning, and the spawn-time re-check tell you *which* code runs. The
[manifest gates and cleared-environment spawn](./plugin-sandbox.md) bound what that code can reach.
Together they are the plugin trust story; neither substitutes for the other.

## Related docs

- [Plugin capability sandbox](./plugin-sandbox.md) — capability confinement after spawn.
- [OS process sandboxing](./os-sandbox.md) — opt-in confinement of the raw plugin process.
- [Using plugins](../plugins/using-plugins.md) — pack install, pinning, rollback, and local dev install.
- [Credentials and secrets](./credentials.md) — plugin OAuth tokens and secret redaction.
