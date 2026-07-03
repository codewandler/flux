---
id: D-48
title: Enforced pin/rollback — spawn-time hash verification over the versioned store
pillar: Core
status: ready
priority: 9
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
- [ ] `flux plugin pin <name> <version>` ensures that version is present in the versioned store
      (fetching it through the same verified D-47 path if absent), repoints the descriptor's
      `program` at it, records its `sha256` + `version`, and remembers the prior version in a new
      `previous` descriptor field. Pinning a version the index does not offer fails cleanly. Test
      `pin_switches_descriptor_to_stored_version`.
- [ ] `flux plugin rollback <name>` repoints to `previous` — **offline and instant** (side-by-side
      store, no download); with no `previous` it is a clean error explaining what rollback now
      means. Test `rollback_flips_to_previous_version_offline`.
- [ ] **Spawn-time enforcement**: any descriptor carrying a `sha256` is re-hashed before
      `PluginHost::spawn` on every load path (agent startup discovery, `flux plugin call`,
      `status`); a mismatch is a hard refusal naming plugin, expected, and actual hash — never a
      silent fallback. Hashless (dev/local) descriptors spawn as today but remain labeled
      `unverified (local)`. Failing-first test `spawn_refuses_hash_drift` (tamper the stored binary
      → load refuses; untampered → loads).
- [ ] `flux plugin status` gains the verification column: `verified` / `hash drift` /
      `unverified (local)`, alongside a version-agreement check (manifest `version` vs descriptor
      `version` — disagreement reported loudly, not fatal). Test `status_reports_hash_drift`.
- [ ] `flux plugin uninstall <name> --purge` also removes `~/.flux/plugins/bin/<name>/` (the
      versioned store dir); without `--purge`, behavior is unchanged. Test
      `uninstall_purge_removes_versioned_store`.
- [ ] Root gate green; no new crate; the enforcement lives beside the descriptor/pack code in
      `crates/flux-plugin` with flux-cli wiring only.

## Progress
- (not started)

## Notes
- Depends on [D-47](D-47-remote-plugin-install.md) (versioned store + descriptor `version`/`sha256`
  fields + verified fetch path).
- Hashing 1–4 MB binaries is sub-millisecond — per-spawn verification is affordable; do it in the
  shared load path so no caller can skip it (no-bypass, same discipline as `Executor::dispatch`).
- The semantic change to `rollback` (was: clear the advisory pin; becomes: flip to `previous`) is a
  deliberate clean cutover — update the subcommand help text in the same change.
- Design sections: "Security model & supply chain" (steps 4–5), "CLI surface (clean cutover)".
