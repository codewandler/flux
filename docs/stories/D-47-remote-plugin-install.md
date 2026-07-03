---
id: D-47
title: Remote `flux plugin install <name>[@version]` — verified fetch into a versioned store
pillar: Core
status: done
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
- [x] `flux plugin install <name>[@<version>] …` (multiple names; `--all` for the whole pack):
      resolves the newest `plugins-v*` release (or the exact tag for `@<version>`), fetches
      `plugins-index.json` + `.minisig`, verifies the signature against a **public key embedded in
      the binary** (`minisign-verify`), downloads the target's archive, verifies its **sha256**
      against the index *before* unpacking, installs to
      `~/.flux/plugins/bin/<name>/<version>/flux-plugin-<name>[.exe]`, and writes the descriptor
      with new `version`, `sha256`, `source` fields (serde-defaulted — existing descriptors stay
      valid). Re-installing a present version is an idempotent no-op with a note.
- [x] Verification is fail-closed with **no bypass flag**: failing-first tests
      `remote_install_refuses_bad_index_signature` (tampered index → hard error, nothing written)
      and `remote_install_refuses_checksum_mismatch` (tampered archive → hard error, nothing made
      executable). Tests run against fixture indexes/archives via an injectable fetcher seam — no
      network in the gate.
- [x] The **asset-name invariant** holds: download URLs are constructed only from
      `(repo, tag, asset-name)`; an index entry whose asset value is URL-shaped or fails the D-35
      name sanitizer is rejected. Test `index_assets_are_bare_names_never_urls`.
- [x] Protocol compatibility: an index whose `protocol` ≠ `flux_plugin::PROTOCOL` is refused with an
      actionable message. Test `install_refuses_protocol_mismatch`.
- [x] Happy-path test `remote_install_writes_versioned_store_and_descriptor` (fixture pack → store
      layout + descriptor fields asserted; `flux plugin ls`/`status` show the version and a
      `verified` marker, while hashless dev descriptors show `unverified (local)`).
- [x] **`--dir` cutover**: the current directory scan becomes `flux plugin install --dir [path]`
      (default `plugins/target/release`); bare `flux plugin install` with no names and no `--dir` is
      an error naming both modes. The scan becomes `.exe`-aware (today
      `plugin_binaries_in` skips every Windows binary because it drops names containing `.` —
      `crates/flux-cli/src/main.rs:5452`); extend `plugin_binaries_in_picks_flux_plugin_executables`.
- [x] Code placement per the design: index schema + verification + versioned store as a `pack`
      module in `crates/flux-plugin` (only new dep: `minisign-verify`); flux-cli keeps UX only.
      Root gate green (build/test/clippy/fmt + flux-codegate layering).
- [x] One real `flux plugin install` verified against a **live** `plugins-v` release —
      2026-07-03: `MINISIGN_SECRET_KEY` set via `gh secret set`, `release-plugins.yml` publish run
      https://github.com/codewandler/flux/actions/runs/28676597169 created `plugins-v0.1.0` (87
      assets, signed index), then `flux plugin install gitlab` → `installed `gitlab` 0.1.0 →
      ~/.flux/plugins/bin/gitlab/0.1.0/flux-plugin-gitlab (verified, source plugins-v0.1.0)` —
      the full trust ladder (signature → protocol → sha256 → versioned store → descriptor) live.

## Progress
- 2026-07-03 — Picked up (in-progress), running in parallel with the D-46 session (release
  pipeline, `plugins/pack-index/`). Schema contract verified against BOTH the design doc and
  D-46's WIP generator: matches, except the generator emits no `description` field — the D-47
  parser treats it as optional (serde-default) so either form reads.
- 2026-07-03 — Implemented and gate-green. New `pack` module in `crates/flux-plugin/src/pack.rs`:
  `Index`/`PluginEntry`/`Artifact` schema types (matching D-46's `pack-index` generator exactly,
  `description` serde-defaulted per the earlier Progress note); `Fetcher` trait (`list_release_tags`
  + `fetch_release_asset`, scoped to `(repo, tag, asset)` only — never a caller URL) with a real
  `GithubFetcher` (unauthenticated GitHub API + direct `releases/download` URLs); minisign
  verification via `minisign-verify` against the embedded `PUBLIC_KEY`, fail-closed, no bypass;
  sha256 checksum of the archive verified against the index entry *before* any unpack; `.tar.xz`
  unpacking via `tar` + pure-Rust `lzma-rs` (no C toolchain / system liblzma needed — cross-checked
  against a real `tar -cJf`-produced archive), `.zip` via the `zip` crate (default-features off,
  `deflate` only — cross-checked against a real `7z a`-produced archive, matching what
  `release-plugins.yml`'s Windows leg produces); `install_many`/`install_one` orchestrate resolve →
  verify → download → checksum → unpack → `add_descriptor` end to end, including the idempotent
  no-op path (same version + file present ⇒ skip the fetch, keep the existing descriptor's hash).
  `PluginDescriptor` grew `version`/`sha256`/`source` (all `#[serde(default)]`, `Default` derived) —
  every existing construction site (8 across `flux-plugin`/`flux-cli`) updated with
  `..Default::default()`; old descriptors still parse. flux-cli's `PluginAction::Install` now takes
  `names: Vec<String>` + `--all` + `--dir [path]`: `--dir` is the pre-D-47 local scan (unchanged
  behavior, now explicit); no `--dir` + names/`--all` calls `pack::install_many`; bare `install`
  (neither) is a clean error naming both modes; `--dir` combined with names/`--all` is also refused.
  Fixed `plugin_binaries_in` (`crates/flux-cli/src/main.rs`) to recognize `flux-plugin-<name>.exe`
  (previously every Windows binary was skipped because the function dropped any name containing
  `.`) while still skipping sidecars (`*.d`, `*.exe.d`). `ls`/`status` gained a version column and a
  `verified`/`unverified (local)` marker (`plugin_verification_label`, derived from whether the
  descriptor carries a `sha256` — display only; spawn-time hash *enforcement* is D-48, deliberately
  not implemented here).

  New deps: runtime — `minisign-verify` (pure Rust, zero deps, as scoped), plus `tar`, `lzma-rs`,
  `zip` (default-features off, `deflate` only) for archive unpacking, added to the root
  `[workspace.dependencies]` and `crates/flux-plugin/Cargo.toml`; dev-only — `minisign` (signs
  fixtures + the one-shot keypair generator, never a runtime dep). `tar` was already resolved in the
  lockfile (an `ort`/`fastembed` transitive behind the optional `local-embeddings` feature); the
  others are genuinely new. Chose pure-Rust `lzma-rs` over `xz2` (liblzma C bindings) to avoid a
  system/vendored liblzma dependency for something host-side, hermetic-test-critical, and small.

  Failing-first tests, all in `crates/flux-plugin/src/pack.rs` (hermetic — an in-memory `MockFetcher`
  fixture, real minisign signing via the `minisign` dev-dep, real tar+xz/zip fixtures built in pure
  Rust, no network): `remote_install_refuses_bad_index_signature`,
  `remote_install_refuses_checksum_mismatch`, `index_assets_are_bare_names_never_urls`,
  `install_refuses_protocol_mismatch`, `remote_install_writes_versioned_store_and_descriptor` (also
  asserts the idempotent-no-op re-install path and that the archive is never re-fetched), plus
  `remote_install_refuses_url_shaped_asset_end_to_end` (the asset-name invariant exercised through
  the full pipeline, not just the unit check), `unpack_single_binary_reads_windows_zip_archives`,
  `resolve_release_tag_picks_highest_semver_from_plugins_v_tags`, `embedded_public_key_parses`. In
  `crates/flux-cli/src/main.rs`: extended `plugin_binaries_in_picks_flux_plugin_executables` with
  `.exe`/sidecar cases, plus new `plugin_install_bare_errors_naming_both_modes`,
  `plugin_install_dir_rejects_combination_with_names_or_all`,
  `plugin_install_dir_scan_registers_unverified_local_descriptor`,
  `plugin_status_marks_hash_carrying_descriptors_verified`. Every test above was bite-verified: the
  guarded logic was temporarily neutered (return `Ok`/skip the check/hardcode the label) one at a
  time and the corresponding test was confirmed to fail for the right reason before being restored.

  **Production keypair minted** via the ignored helper test
  (`cargo test -p flux-plugin --lib pack::tests::generate_pack_keypair -- --ignored --nocapture`).
  Public key embedded as `flux_plugin::pack::PUBLIC_KEY`:
  `RWSd30xfPYIFZc6x0bb9KukLrw2ax49cKMbP6bKpj5wpACesSqZE1qcp`. Secret key written to
  `~/.flux/minisign-pack.key` (mode `0600`, outside the repo, never committed). **Operator action
  needed to unblock the live-release acceptance item above**: read that file's full contents
  (`untrusted comment:` line + the base64 line) and add it verbatim as the `MINISIGN_SECRET_KEY`
  GitHub Actions secret on `codewandler/flux`, then run `release-plugins.yml` with `publish: true`.
  Key rotation later = generate a new keypair the same way, embed the new public key, ship a flux
  release (per the design doc's residual-risk note).

  Gate: `cargo test -p flux-plugin -p flux-cli` green (57+76 tests, 1 intentionally `#[ignore]`d);
  `cargo clippy -p flux-plugin -p flux-cli --all-targets -- -D warnings` clean; `cargo fmt -p
  flux-plugin -p flux-cli` applied; `cargo build --workspace` clean; `cargo test --workspace` green
  (0 failures across every crate); `cargo test -p flux-codegate` green (layering intact — `pack` is
  ordinary code inside the existing L4 `flux-plugin`, no new crate); `cargo fmt --all -- --check`
  clean. `plugins/` (the nested, gate-excluded workspace) was not touched.

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
