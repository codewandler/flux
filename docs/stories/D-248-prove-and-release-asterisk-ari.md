---
id: D-248
title: "Prove and release the complete Asterisk ARI plugin"
pillar: Agent
status: done
priority: 8
epic: asterisk-ari
design: docs/designs/asterisk-ari.md
areas: [plugins]
note: "109/109 Swagger coverage, 8/8 AMI preservation, docs/live smoke, root+pack gates and plugin-pack release"
---

# Prove and release the complete Asterisk ARI plugin

## Goal

Turn the completed implementation into independently reviewable evidence and a plugin-pack release
operators can install.

## Acceptance

- [x] A two-way census proves 109 official ARI operations are accounted for; no selector sweep,
      skipped route or undocumented generated operation exists.
- [x] All eight existing AMI operation identities and schemas are byte-pinned and unchanged.
- [x] Plugin README, generated skill and env-gated live smoke document both AMI and ARI setup.
- [x] Root and nested workspace gates, protocol drift checks and a clean-tree final diff are green.
- [x] Any protocol-line/core release required by D-241 lands before a plugin-pack release; the signed
      pack workflow publishes the new Asterisk binary and its index entry successfully.

## Progress

- 2026-08-02: failing-first `cargo test -p asterisk --test ari_completion` rejected the initial
  census because it classified the two plugin lifecycle controls under the official vendor
  namespace. D-247's exported `EVENT_READ_CONTROL`/`EVENT_CLOSE_CONTROL` constants now keep
  `asterisk.ari.control.events.read` and `.close` visibly separate from the 109 Swagger facts.
- The completion test requests the manifest from the real `flux-plugin-asterisk` binary with a
  cleared environment. It proves in both directions that the manifest carries 108 generated REST
  operations, the one official `events.eventWebsocket` fact and exactly the two non-vendor lifecycle
  controls, with no duplicate name. A second test byte-compares all eight existing AMI input schemas
  and identities and proves none gained a different output-schema contract.
- `plugins/asterisk/README.md` now documents AMI and ARI installation/configuration, host-injected
  Basic auth, the default `ASTERISK_ARI_URL`, scoped private-network grants, all 109 official facts,
  event open/read/close lifecycle and host-to-blob stored recordings. `scripts/smoke-plugins.sh` adds
  a credential-presence-gated `asterisk.ari.asterisk.ping` call in its isolated registry/config and
  never embeds either credential in the operation input or prints either value.
- An isolated `flux plugin skill --install --global` run rendered the Asterisk reference beneath a
  temporary `HOME` and proved it carries the AMI ping and credentials, ARI ping and endpoint, the
  official event WebSocket, and both reviewed `asterisk.ari.control.events.*` lifecycle controls.
  The smoke script now repeats that generated-skill assertion before its env-gated live calls.
- Scoped verification passed: `cargo build -p asterisk`, `cargo test -p asterisk`,
  `cargo clippy -p asterisk --all-targets -- -D warnings`,
  `cargo fmt --manifest-path plugins/Cargo.toml -p asterisk -- --check`, and
  `bash -n scripts/smoke-plugins.sh`. The completion target passed six tests, including the two D-248
  assertions and four shared executor tests.
- Coordinator integration passed root and nested workspace build/test/clippy/fmt gates and
  `git diff --check`. The released source tree is exact commit
  `7270b2f75fda9bd3f1e9b21bbad7531886e6c5f3`; the only local dirt after verification is the
  unrelated, user-owned C-163 story edit.
- The first release-cut gate exposed a pre-existing process-global environment race in
  `metadata::tests::load_config_in_reads_the_pinned_home_and_never_the_process_home`: a sibling test
  could temporarily supply `FLUX_MANAGED_CONFIG = org-default` while this reader asserted an empty
  managed layer. The failing full-workspace run is the regression evidence; taking the existing
  `MANAGED_CONFIG_LOCK` made all 182 `codewandler-flux-runtime` library tests pass together.
- Core candidate/release workflow `30746037338` and crates.io workflow `30746037327` succeeded for
  `v0.51.1` at the exact commit above. The GitHub release has 28 assets and valid provenance for all
  14 executable assets; `codewandler-flux-plugin-protocol@1.3.0` is live.
- Plugin dry run `30746468440` and publish run `30746595522` succeeded at the same SHA. The
  workflow-created `plugins-v0.1.6` tag points there and its signed index records 19 plugins across
  five targets: 95 archives plus `plugins-index.json` and its minisign signature, with zero release
  digest/size mismatches and five Asterisk archives. `codewandler-flux-host-kit@1.2.0` is live with
  protocol requirement `^1.3`; protocol-drift and released-pack wire-compatibility checks passed.
- An isolated remote install reported Asterisk `v0.1.6` as `[ok] [verified]`, exposed 120 total
  operations and generated `SKILL.md` plus the Asterisk reference. The live read-only ARI ping was
  not attempted because the three required `ASTERISK_ARI_*` values were absent; the committed smoke
  remains credential-presence-gated and does not print them.
