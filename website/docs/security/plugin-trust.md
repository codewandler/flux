---
title: Plugin trust & signing
description: How Flux distinguishes signed-pack, Git-source, and local plugin installs, what each mode verifies, and which risks remain.
---

# Plugin trust & signing

Plugin trust is the supply-chain side of plugin safety: proving which binary is installed and which
binary is spawned. It is separate from the capability sandbox, which controls what that binary can
reach through Flux once it is running.

The everyday install, pin, rollback, and uninstall commands live in
[Using plugins](../plugins/using-plugins.md).

## The three trust modes

| Source | Install command | Status label | What Flux can verify |
|---|---|---|---|
| Signed pack | `flux plugin install <name>` or `--all` | `verified` | Signed release index, archive SHA-256, recorded binary SHA-256, and a hash re-check at every spawn. |
| Git source | `flux plugin install --git <url> …` | `from-source (unverified)` | The Git URL and resolved commit are displayed before the build and recorded afterward. No signed-pack hash is recorded. |
| Local binary | `flux plugin install --dir[=<path>]` or `flux plugin add …` | `unverified (local)` | The registered path only. No version or hash is recorded. |

### Signed pack verification

The integration plugins ship separately from Flux as a **signed pack** (the `plugins-v*` releases).
Every install from the pack is verified end to end and **fails closed — there is no bypass flag**:

1. **Fixed origin.** Every download URL is built only from `(repo, tag, bare-asset-name)` against
   `github.com`. Asset names are validated to be bare (not URL- or path-shaped) and to carry the
   `flux-plugin-` prefix, so a tampered index can never redirect a download somewhere else.
2. **Signed index.** The release's `plugins-index.json` is **minisign-verified** against a public key
   **embedded in the Flux binary** before a single byte of it is trusted. There is no
   `--skip-signatures`; a bad signature is a hard error and nothing is fetched.
3. **Checksum before executable.** Each archive's **SHA-256** is checked against the (now-trusted)
   index entry *before* it is unpacked and made executable. A mismatch means the archive is never
   written to disk.
4. **Install-time recording.** The installed binary's version and SHA-256 are recorded, along with a
   hash sidecar beside the stored binary, in the versioned store
   `~/.flux/plugins/bin/<name>/<version>/`.

### Git source builds

`flux plugin install --git <url>` clones a repository, resolves the requested default ref, tag,
revision, or branch to a commit, and proposes a `cargo build --release --locked`. **Building source
is arbitrary code execution**: Cargo may run build scripts and procedural macros before a plugin
binary exists.

Flux therefore displays the URL and resolved commit and requires an explicit `[y/N]` confirmation
before the build. `FLUX_ALLOW_SOURCE_BUILD=1` is the non-interactive consent channel; it does not
verify the repository. The installed descriptor records the Git URL and commit, is labelled
`from-source (unverified)`, and carries no signed-pack SHA-256. Editing or replacing that built binary
later is not detected by the signed-pack spawn-time check. Review the source like any other native
dependency and prefer an exact `--rev` when reproducibility matters.

### Local binaries

`flux plugin install --dir` scans already-built `flux-plugin-*` binaries, while
`flux plugin add <name> <program>` registers one explicit executable. Both modes trust your local
build or supplied path and record no version or SHA-256. Flux labels them `unverified (local)`.

## Enforcement at every spawn

For signed-pack installs, recording the hash at install time isn't enough — Flux **re-hashes the
binary every time it spawns it** and refuses to launch on any drift. A `HashDrift` names the
expected and actual hash and stops
the run; it is never a silent fallback. This is also what makes `pin` and `rollback` safe: they are
verified switches over the side-by-side versioned store, and `rollback` refuses an entry that has no
recorded hash rather than blessing unverified bytes.

Git-source and local descriptors have no recorded SHA-256, so they do not receive this integrity
check. Their distinct status labels keep that boundary visible.

## What this does — and does not — guarantee

> The trust ladder proves *which* code runs. It does not sandbox what that code does.

- **For a verified pack install, it does guarantee** that the binary you run is exactly the one the
  signed release published, byte for byte, and has not been swapped or corrupted since install.
- **For Git-source and local installs, it does not make that guarantee.** Their provenance or path is
  recorded as described above, but the resulting binary is unverified.
- **It does not guarantee** the code is harmless. Plugin binaries are **trusted native code — not
  OS-sandboxed by default.**
  Confinement of what the code can reach through Flux is the
  [capability sandbox](./plugin-sandbox.md)'s job (cleared environment, deny-by-default manifest,
  host-does-all-IO); opt-in [OS process sandboxing](./os-sandbox.md) additionally confines what the
  raw binary's syscalls can reach on disk and network. Review installed plugins the way you review
  dependencies.
- **Residual risk, stated honestly**: a compromised repository or CI pipeline could sign a malicious
  artifact, and the signature check would pass. The signing key lives only in a CI secret, never in
  the repo; stronger provenance (build attestation) is a named future hardening, not yet in place.

For verified installs, the signed pack, SHA-256 pinning, and spawn-time re-check tell you *which*
code runs. The [manifest gates and cleared-environment spawn](./plugin-sandbox.md) bound what that
code can reach.
Together they are the plugin trust story; neither substitutes for the other.

## Related docs

- [Plugin capability sandbox](./plugin-sandbox.md) — capability confinement after spawn.
- [OS process sandboxing](./os-sandbox.md) — opt-in confinement of the raw plugin process.
- [Using plugins](../plugins/using-plugins.md) — signed-pack, Git-source, and local installation.
- [Credentials and secrets](./credentials.md) — plugin OAuth tokens and secret redaction.
