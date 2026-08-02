---
id: D-243
title: "Compile Asterisk Swagger into exact plugin operation contracts"
pillar: Agent
status: done
priority: 3
epic: asterisk-ari
design: docs/designs/asterisk-ari.md
areas: [plugins]
note: "Swagger 1.1/1.2 parser, 85 model schemas, exact parameters, reviewed risk table and generic REST executor"
---

# Compile Asterisk Swagger into exact plugin operation contracts

## Goal

Turn the vendored legacy Swagger into executable, discoverable ARI operations without hand-copying
routes or weakening their input/output contracts.

## Acceptance

- [x] Failing-first tests compare every generated identity/method/path/parameter/response model with
      the vendored source in both directions.
- [x] Input schemas are closed and preserve required, primitive/list, path/query/body and enum
      declarations; output schemas resolve all 85 model declarations and inheritance.
- [x] A generic executor encodes only declared inputs, rejects the WebSocket route, injects Basic auth
      host-side and handles JSON, void and binary response classes truthfully.
- [x] Every operation has reviewed risk, idempotency and semantic effects; DELETE is never inferred as
      an ordinary medium-risk write.
- [x] Generator parse errors refuse without changing committed output; generated registration errors
      reach fallible plugin startup, and production neither reads nor embeds the upstream Swagger text.

## Evidence

- Failing first: `cd plugins && cargo test -p asterisk --test ari_generated_contracts` failed because
  `asterisk/src/ari.rs` did not exist before implementation.
- `cd plugins && cargo test -p asterisk` passed 18 binary tests, 11 generated-contract tests and 5
  vendored-spec tests.
- `cd plugins && asterisk/scripts/generate-ari-contracts.py --check` passed with no output.
- `cd plugins && cargo build -p asterisk` passed.
- `cd plugins && cargo clippy -p asterisk --all-targets -- -D warnings` passed.
- `cd plugins && cargo fmt -p asterisk -- --check` passed.
- `cd plugins && cargo test -p codewandler-flux-host-kit --test guest_dependency_boundary` passed
  its one boundary test.
