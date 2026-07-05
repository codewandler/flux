---
id: D-48
title: Enforced pin/rollback — spawn-time hash verification over the versioned store
pillar: Core
status: done
epic: plugin-platform-hardening
design: docs/designs/plugin-distribution.md
note: "turn `flux plugin pin`/`rollback` from advisory labels into supply-chain statements: pin fetches+repoints+records hash, rollback is an offline flip to `previous`, and a sha256-carrying descriptor is re-hashed before every spawn (drift = hard refusal); `status` gains the verification column"
---

# Enforced pin/rollback — spawn-time hash verification over the versioned store

## Goal
Make the existing `flux plugin pin`/`rollback` surface *mean* something. Today `pinned` is an
advisory string surfaced by `ls` (`PluginDescriptor.pinned`, `crates/flux-plugin/src/lib.rs:2063`) —
nothing enforces it at spawn and nothing ties a descriptor to the bytes that were installed. With
the D-47 versioned store in place, pin/rollback become verified version switches and the recorded
hash is enforced every time the plugin runs (trust-ladder step 5 of
[plugin distribution](../designs/plugin-distribution.md)).

## Acceptance
- [x] `flux plugin pin <name> <version>` ensures that version is present in the versioned store
      (fetching it through the same verified D-47 path if absent), repoints the descriptor's
      `program` at it, records its `sha256` + `version`, and remembers the prior version in a new
      `previous` descriptor field. Pinning a version the index does not offer fails cleanly. Test
      `pin_switches_descriptor_to_stored_version`.
      Evidence: `flux_plugin::pack::pin` (`crates/flux-plugin/src/pack.rs`) — an already-stored
      version with a hash sidecar repoints **offline** (the test proves the archive is not
      re-downloaded); anything else rides `install_many` (signed index + checksum), so an
      unoffered version fails in `resolve_release_tag` with the descriptor untouched.
- [x] `flux plugin rollback <name>` repoints to `previous` — **offline and instant** (side-by-side
      store, no download); with no `previous` it is a clean error explaining what rollback now
      means. Test `rollback_flips_to_previous_version_offline`.
      Evidence: `pack::rollback` is a **sync fn with no fetcher parameter** — offline by
      construction. Current/previous swap, so a second rollback flips forward again (tested).
      `set_pinned` (the old advisory pin/clear) is deleted — clean cutover; CLI help rewritten.
- [x] **Spawn-time enforcement**: any descriptor carrying a `sha256` is re-hashed before
      `PluginHost::spawn` on every load path (agent startup discovery via
      `load_plugin_manifests`, `flux plugin call`, `status`'s probe); a mismatch is a hard refusal
      naming plugin, expected, and actual hash. Hashless descriptors spawn as today, labeled
      `unverified (local)`. Test `spawn_refuses_hash_drift` (`crates/flux-plugin/tests/host.rs`):
      copies the real echo-plugin binary, spawns verified with its recorded hash, **appends bytes
      to the binary**, and asserts the refusal names plugin + both hashes; the hashless descriptor
      still spawns over the tampered file.
      Evidence: `PluginHost::spawn_verified` + `flux_plugin::verify_descriptor`
      (`Verification::{Verified, HashDrift, UnverifiedLocal}`); an unreadable binary under a
      recorded hash is drift, never a silent pass. All three flux-cli descriptor spawn sites
      route through it.
- [x] `flux plugin status` gains the verification column: `verified` / `hash drift` /
      `unverified (local)`, alongside a version-agreement check (manifest `version` vs descriptor
      `version` — disagreement reported loudly via a yellow warning line, not fatal). Test
      `status_reports_hash_drift` (flux-cli). On drift the doomed liveness probe is skipped
      (`unloadable: refused: hash drift`). Bonus: `flux plugin ls` re-hashes too (sub-ms each) —
      the old descriptor-field-only label could report `verified` over tampered bytes
      (`plugin_status_rehashes_hash_carrying_descriptors` pins the inversion: the D-47 test
      asserted `verified` for an unreadable `deadbeef` descriptor; it now asserts drift).
- [x] `flux plugin uninstall <name> --purge` also removes `~/.flux/plugins/bin/<name>/` (the
      versioned store dir); without `--purge`, behavior is unchanged. Test
      `uninstall_purge_removes_versioned_store` (also pins: `--purge` cleans an orphaned store
      dir whose descriptor is already gone). `pack::purge_store` carries the D-35 traversal guard
      (added to `descriptor_path_rejects_traversal_names`).
- [x] Root gate green; no new crate; the enforcement lives beside the descriptor/pack code in
      `crates/flux-plugin` with flux-cli wiring only.
      `cargo test --workspace` (89 suites ok, 0 failed) · `clippy --workspace --all-targets
      -D warnings` clean · `fmt --all --check` clean · flux-codegate 4/4. Plugins workspace
      untouched by this story.

## Progress
- 2026-07-05 — implemented and gated in one session (design was already settled in
  `docs/designs/plugin-distribution.md` §"Security model" step 5 + §"CLI surface").
  - **The offline-hash problem and its answer**: `previous` is a plain version string (the
    acceptance wording), but an *enforced* offline flip needs that version's hash from somewhere
    trustworthy — re-hashing whatever bytes sit in the store would bless a tampered binary
    (hash laundering). Answer: `install_one` writes a **hash sidecar**
    (`<store>/<name>/<version>/flux-plugin-<name>.sha256`) at verified-unpack time; offline
    `pin`/`rollback` read it; a missing sidecar (pre-D-48 store entry) is a clean refusal naming
    `flux plugin pin <name> <version>` as the re-record path.
  - Deviations/extensions vs the story letter (all recorded in Acceptance): rollback **swaps**
    current↔previous (round-trip); `install_one` records `previous` on any version switch (not
    only pin), so rollback also covers a plain-install upgrade; `ls` re-hashes like `status`;
    `status` skips the spawn probe on drift; pin preserves operator-set `args` across switches;
    pin's offline path stamps `source` as `plugins-v<version>` (faithful: the pack is released
    lockstep).
  - Failing-first honesty: the API-level tests fail-to-compile against the pre-change tree (the
    D-53 precedent); the one true behavioral inversion is pinned by the rewritten D-47 label test
    (old: `verified` from the descriptor field alone; now: re-hash → drift).

## Notes
- Depends on [D-47](D-47-remote-plugin-install.md) (versioned store + descriptor `version`/`sha256`
  fields + verified fetch path).
- Hashing 1–4 MB binaries is sub-millisecond — per-spawn verification is affordable; do it in the
  shared load path so no caller can skip it (no-bypass, same discipline as `Executor::dispatch`).
- The semantic change to `rollback` (was: clear the advisory pin; becomes: flip to `previous`) is a
  deliberate clean cutover — update the subcommand help text in the same change.
- Design sections: "Security model & supply chain" (steps 4–5), "CLI surface (clean cutover)".
