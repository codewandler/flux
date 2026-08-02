---
id: D-245
title: "Ship ARI bridges, playbacks and sounds resources"
pillar: Agent
status: done
priority: 5
epic: asterisk-ari
design: docs/designs/asterisk-ari.md
areas: [plugins]
note: "bridge and playback operations affect live media and carry explicit high/destructive review"
---

# Ship ARI bridges, playbacks and sounds resources

## Goal

Expose every bridge, playback and sound operation with live-media consequences represented in the
safety contract.

## Acceptance

- [x] Every Swagger operation in `bridges`, `playbacks` and `sounds` is present exactly once.
- [x] Failing-first tests pin request encoding and high/destructive classifications for live bridge
      and playback mutations.
- [x] Representative output fixtures validate resolved model/list schemas without erasing unknown
      vendor fields.

## Evidence

- Failing first: `cd plugins && cargo test -p asterisk --test ari_bridge_media_resources
  every_bridge_playback_and_sound_operation_is_present_once` failed with `bridges` measured at 18
  against the deliberately incomplete zero-operation fixture.
- `cd plugins && cargo test -p asterisk --test ari_bridge_media_resources` passed all 8 tests,
  including the four generic-executor tests included from `ari.rs` and four resource proofs.
- The proof reads the three vendored documents and matches the generated manifest in both directions:
  bridges 18, playbacks 3, sounds 2 (23 total).
- `cd plugins && cargo build -p asterisk` and
  `cargo clippy -p asterisk --all-targets -- -D warnings` passed.
- `cd plugins && rustfmt --edition 2021 --check
  asterisk/tests/ari_bridge_media_resources.rs` passed.
- The whole `cargo test -p asterisk` run passed D-245 and every completed test binary, then failed
  only the concurrent D-244 placeholder in `ari_system_resources.rs` (`0` against expected `36`).
  Whole-package `cargo fmt --check` likewise reports only concurrent D-246 formatting in
  `ari_channel_resources.rs`; D-245 does not edit either file.
