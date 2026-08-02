---
id: D-240
title: "Vendor and inventory the official Asterisk ARI Swagger set"
pillar: Agent
status: done
epic: asterisk-ari
design: docs/designs/asterisk-ari.md
areas: [plugins]
note: "pin Asterisk 22.10.1: resources.json plus 11 declared documents; deterministic provenance and exact census"
---

# Vendor and inventory the official Asterisk ARI Swagger set

## Goal

Make the complete upstream ARI contract a reproducible, reviewable and offline input to the Asterisk
plugin.

## Acceptance

- [x] `resources.json` and every one of its eleven declared `api-docs` documents are vendored from
      tag `22.10.1` (annotated tag object `4f85d05889cf9fb9c9e2ae44cc3f4a825a74545a`, peeled commit
      `f0e408a7b0d829c85bf15fa4b487870a50cb3000`) with SHA-256 provenance.
- [x] A deterministic vendor script supports an exact local-source replay, refuses a missing/extra
      document, and never fetches an unpinned ref.
- [x] Failing-first tests prove the resource inventory, 76 paths, 109 operations, 85 models,
      275 parameters, 108 REST operations and one WebSocket operation from the vendored bytes.
- [x] The test rejects source-tag/hash drift and verifies that no credential, private endpoint or
      personal example value entered the vendored set.

## Progress

- 2026-08-02: counts measured with `jq` over all eleven documents; upstream ref resolved with
  `git ls-remote --tags`.
- 2026-08-02: corrected the upstream identity: `git ls-remote
  https://github.com/asterisk/asterisk.git 'refs/tags/22.10.1*'` reports annotated tag object
  `4f85d05889cf9fb9c9e2ae44cc3f4a825a74545a` and peeled commit
  `f0e408a7b0d829c85bf15fa4b487870a50cb3000`. Raw bytes are fetched only through the peeled commit;
  provenance pins both.
- 2026-08-02: failing-first
  `cargo test -p asterisk --test ari_vendored_specs
  vendored_contract_has_the_exact_official_inventory_and_census -- --exact` exited 101 on the absent
  `specs/ari-22.10.1/resources.json`; after vendoring,
  `cargo test -p asterisk --test ari_vendored_specs -- --nocapture` passed 4 tests.
- 2026-08-02: an exact `--source-dir` replay produced byte-identical hashes. Replays with one extra
  `extra.json` and with `sounds.json` missing both exited non-zero and named the inventory difference.
- 2026-08-02: `ari-22.10.1.sha256` is the independent trust anchor used by both fetch modes and the
  Rust integrity test. A local replay with only `resources.json`'s `apiVersion` changed from
  `10.0.0` to `10.0.1` exited non-zero with the expected and actual SHA-256; the checked test
  `one_altered_source_byte_is_rejected_by_the_pinned_hashes` proves the same failure in the gate.
- 2026-08-02: scoped gate green: `cargo build -p asterisk`; `cargo test -p asterisk` (13 existing
  AMI tests plus 5 vendored-contract tests); `cargo clippy -p asterisk --all-targets -- -D warnings`;
  `cargo fmt -p asterisk -- --check`; and
  `cargo test -p codewandler-flux-host-kit --test guest_dependency_boundary`.
