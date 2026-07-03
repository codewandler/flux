---
id: D-47
title: Remote `flux plugin install <name>[@version]` — verified fetch into a versioned store
pillar: Core
status: in-progress
epic: plugin-platform-hardening
design: docs/designs/plugin-distribution.md
note: "the demand side: resolve the `plugins-v` release → verify minisign-signed index (embedded pubkey, no skip flag) → sha256-check → unpack to `~/.flux/plugins/bin/<name>/<version>/` → descriptor records version+sha256+source; local scan moves behind `install --dir`"
---

# Remote `flux plugin install <name>[@version]` — verified fetch into a versioned store

## Goal
A flux user without the source tree runs `flux plugin install gitlab slack` and gets working,
verified plugins. This is the demand side of
[plugin distribution](../designs/plugin-distribution.md): resolve → verify → store → register, with
the trust ladder enforced end-to-end and the old local-dir scan moved behind an explicit flag (clean
cutover).

## Acceptance
- [ ] `flux plugin install <name>[@<version>] …` (multiple names; `--all` for the whole pack):
      resolves the newest `plugins-v*` release (or the exact tag for `@<version>`), fetches
      `plugins-index.json` + `.minisig`, verifies the signature against a **public key embedded in
      the binary** (`minisign-verify`), downloads the target's archive, verifies its **sha256**
      against the index *before* unpacking, installs to
      `~/.flux/plugins/bin/<name>/<version>/flux-plugin-<name>[.exe]`, and writes the descriptor
      with new `version`, `sha256`, `source` fields (serde-defaulted — existing descriptors stay
      valid). Re-installing a present version is an idempotent no-op with a note.
- [ ] Verification is fail-closed with **no bypass flag**: failing-first tests
      `remote_install_refuses_bad_index_signature` (tampered index → hard error, nothing written)
      and `remote_install_refuses_checksum_mismatch` (tampered archive → hard error, nothing made
      executable). Tests run against fixture indexes/archives via an injectable fetcher seam — no
      network in the gate.
- [ ] The **asset-name invariant** holds: download URLs are constructed only from
      `(repo, tag, asset-name)`; an index entry whose asset value is URL-shaped or fails the D-35
      name sanitizer is rejected. Test `index_assets_are_bare_names_never_urls`.
- [ ] Protocol compatibility: an index whose `protocol` ≠ `flux_plugin::PROTOCOL` is refused with an
      actionable message. Test `install_refuses_protocol_mismatch`.
- [ ] Happy-path test `remote_install_writes_versioned_store_and_descriptor` (fixture pack → store
      layout + descriptor fields asserted; `flux plugin ls`/`status` show the version and a
      `verified` marker, while hashless dev descriptors show `unverified (local)`).
- [ ] **`--dir` cutover**: the current directory scan becomes `flux plugin install --dir [path]`
      (default `plugins/target/release`); bare `flux plugin install` with no names and no `--dir` is
      an error naming both modes. The scan becomes `.exe`-aware (today
      `plugin_binaries_in` skips every Windows binary because it drops names containing `.` —
      `crates/flux-cli/src/main.rs:5452`); extend `plugin_binaries_in_picks_flux_plugin_executables`.
- [ ] Code placement per the design: index schema + verification + versioned store as a `pack`
      module in `crates/flux-plugin` (only new dep: `minisign-verify`); flux-cli keeps UX only.
      Root gate green (build/test/clippy/fmt + flux-codegate layering).

## Progress
- 2026-07-03 — Picked up (in-progress), running in parallel with the D-46 session (release
  pipeline, `plugins/pack-index/`). Schema contract verified against BOTH the design doc and
  D-46's WIP generator: matches, except the generator emits no `description` field — the D-47
  parser treats it as optional (serde-default) so either form reads.

## Notes
- Depends on [D-46](D-46-plugin-pack-release-pipeline.md) — the fixture index/archives must encode
  the exact schema D-46 publishes, and one live install against a real `plugins-v` release is the
  final verification step.
- Latest-release resolution uses the GitHub releases listing filtered by the `plugins-v` prefix
  (unauthenticated rate limits are acceptable; `@<version>` needs no API call).
- Reuse: descriptor store + D-35 sanitizer; workspace `reqwest` (rustls) + `sha2` already in
  flux-plugin; D-19 `status` as the reporting surface.
- Design sections: "The pack channel", "Security model & supply chain" (ladder steps 1–4), "CLI
  surface (clean cutover)".
