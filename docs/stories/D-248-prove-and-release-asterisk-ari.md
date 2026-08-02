---
id: D-248
title: "Prove and release the complete Asterisk ARI plugin"
pillar: Agent
status: in-progress
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

- [ ] A two-way census proves 109 official ARI operations are accounted for; no selector sweep,
      skipped route or undocumented generated operation exists.
- [ ] All eight existing AMI operation identities and schemas are byte-pinned and unchanged.
- [ ] Plugin README, generated skill and env-gated live smoke document both AMI and ARI setup.
- [ ] Root and nested workspace gates, protocol drift checks and a clean-tree final diff are green.
- [ ] Any protocol-line/core release required by D-241 lands before a plugin-pack release; the signed
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
- Coordinator-owned work remains unchecked: root/protocol/pack gates, clean-tree integration,
  versioning, changelogs, signing, publication and release verification.
