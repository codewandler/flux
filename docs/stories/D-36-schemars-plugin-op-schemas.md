---
id: D-36
title: schemars-derive every plugin OperationSpec schema (kill hand-written JSON Schema on the plugin side)
pillar: Core
status: in-progress
priority:
epic:
design:
note: the plugin-side continuation of D-34 — ~150 hand-written `so(json!{...}, json![...])` op schemas across 17 in-repo plugins become schemars-derived via new host-kit `read_op_typed`/`write_op_typed` helpers
---

# schemars-derive every plugin OperationSpec schema

## Goal

No plugin `OperationSpec` may hand-write its JSON Schema: every op's `input_schema` must be
derived from a typed Rust struct via `schemars`, so the schema the model sees and the params
the handler parses cannot drift. This is the **plugin-side continuation of [D-34](D-34-schemars-op-schemas.md)**,
which finished the in-process `ToolSpec` ops and explicitly deferred the plugin ops ("tracked as
a separate story"). The deferral is recorded in [`DRIFT.md`](../../DRIFT.md) §"Out of scope".

The same single-source-of-truth principle, just on the L4 plugin side: today each plugin declares
ops with a local `so(props, required)` helper that emits a `json!({...})` object, then the handler
re-extracts fields from a `serde_json::Value` by hand — two copies of each op's contract that can
(and do) drift. After this, a `#[derive(JsonSchema, Deserialize)]` struct is the one source, and
host-kit derives the schema + parses the call in one typed step.

## Acceptance

- [ ] **host-kit** grows `read_op_typed`/`write_op_typed` (or equivalent typed-op helpers) that
      take a schemars-derived struct and produce the manifest `OperationSpec` schema + feed the
      handler a parsed `T` instead of a raw `Value`. `schemars` becomes a `host-kit` dependency
      re-exported to plugins (so plugin crates don't each add it ad hoc).
- [ ] **Every in-repo plugin** (the 17 under `plugins/` — gitlab, grafana, docker, huggingface,
      opsgenie, homer, asterisk, sql, slack, websearch, jira, confluence, kubernetes, loki,
      prometheus, alertmanager, aws) replaces its local `so(json!({...}, json![...]))` op
      declarations + handler `Value` parsing with a typed struct. `rg -n 'fn so\b' plugins/`
      returns **no** plugin `so` definitions (host-kit provides the typed path; any residual
      `so` is a host-kit internal or removed).
- [ ] `rg -c 'so\(' plugins/*/src/*.rs` (excluding `plugins/target/`) shows **no** hand-written
      op-schema sites; the per-plugin `so` helper is deleted from every `main.rs`.
- [ ] A **regression guard** test (mirroring `crates/flux-tools/tests/no_manual_schema.rs`)
      fails on a deliberately reintroduced `so(json!({...}, json![...]))` plugin op and passes
      on the migrated tree. The guard runs in the plugin workspace
      (`cargo test --manifest-path plugins/Cargo.toml`).
- [ ] Plugin manifests at runtime are byte-identical (or demonstrably equivalent) to pre-migration
      for at least the `gitlab` and `sql` plugins — verified against a recorded manifest snapshot
      or an existing plugin-manifest test — so this is a pure schema-derivation refactor, not a
      contract change.
- [ ] Dev loop green: `cargo build --workspace`, `cargo test --workspace`,
      `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all`,
      `cargo test -p flux-codegate`, plus the plugin workspace
      (`cargo build/test --manifest-path plugins/Cargo.toml`).

## Scope

- **In:** the 17 native `plugins/<name>` crates + `plugins/host-kit` (the typed-op helpers +
  `schemars` re-export). The migration pattern is the one D-34 established (`WriteInput`/
  `WriteTool` full-SSoT, or the `#[allow(dead_code)]` schema-only flux-eval precedent where a
  handler must stay ad-hoc).
- **Out:** `flux-lang/src/opspec.rs` (the composite-op schema *generator* — its job is to
  produce schemas programmatically), provider `json!({...})` test/MCP-passthrough sites, and
  `flux-cli/plugin_skill.rs` / `flux-plugin/bin/*` test/example code — all already excluded by
  D-34's `no_manual_schema` guard scope.
- **Not a contract change:** op names, params, effects, and risk levels stay identical; only the
  schema's authorship moves from hand-written `json!` to schemars-derived. Any drift found during
  migration (a field the schema advertised but the handler never read, or vice-versa) is recorded
  in `DRIFT.md` as D-34 did, not silently "fixed" — a contract change is a separate story.

## Progress
- **Infrastructure landed.** `host-kit` grew `read_op_typed::<T>` / `write_op_typed::<T>`
  + `op_input_schema::<T>()`, a `schemars` re-export, and `schemars` joined the plugin workspace
  deps. (`flux-spec::tool_input_schema` is reused, so the plugin and crate sides share one
  derivation path.)
- **`homer` migrated** (8 ops) — the reference plugin: clean `str_opt`/`bool_opt`/`i64_opt`/
  `str_array` extractors, no flex coercion. All `so(...)` sites replaced; the local `so` helper
  is deleted; handlers unchanged. 9 existing handler tests + a new `schema_contract` test
  (asserts the derived schema's fields/required/types/enums match the legacy `so(...)` contract)
  pass.
- **Guard landed.** `plugins/host-kit/tests/no_manual_plugin_schema.rs` (scoped to
  `MIGRATED_PLUGINS = ["homer"]`) fails on a reintroduced `fn so(` / `so(json!{...})` and on a
  partial migration (no `*_op_typed::<`); verified failing-first by reintroducing `so`.
- **Drift collected.** `DRIFT.md` § D-36 records two `homer` drifts (`call.list` handler reads
  `ua`/`method`/`call_id` the schema omits; `call.show` advertises a `render` field the handler
  ignores) — preserved as-is (pure schema-source change), plus schemars representation notes.
- **Remaining (16 plugins):** `gitlab`, `grafana`, `docker`, `huggingface`, `opsgenie`,
  `asterisk`, `sql`, `slack`, `websearch`, `jira`, `confluence`, `kubernetes`, `loki`,
  `prometheus`, `alertmanager`, `aws`. Several use `flex_str`/`flex_i64` string-or-number
  coercion → schema-only struct (handler stays), D-34 precedent. As each migrates: delete its
  `so` helper, add it to `MIGRATED_PLUGINS`, add an inline `schema_contract` test, record any
  drift here.

## Notes
- **Origin:** explicitly deferred by [D-34](D-34-schemars-op-schemas.md) (Scope + Progress) and
  recorded in [`DRIFT.md`](../../DRIFT.md) §"Out of scope (deferred / correctly non-manual)".
- **Unblocked by D-34** (pattern + precedent: `WriteInput`/`WriteTool` full-SSoT; the
  `#[allow(dead_code)]` schema-only struct precedent) and **by [L-09](L-09-named-argument-calls.md)**
  (named-argument calls: `x-param-order` is gone and `required` is a set, so schemars-derived
  schemas bind correctly).
- **Related:** [D-08](D-08-integration-plugin-pack.md) (created `host-kit` and the `so` helper
  this story replaces — mechanism, not schema authorship) and
  [D-10](D-10-process-plugin-protocol.md) (the `OperationSpec` wire shape these schemas fill).
  [D-29](D-29-migrate-plugins-to-references.md) is orthogonal (reference-based IO, not schema
  derivation) but touches the same plugin `main.rs` files — coordinate ordering if both land near
  each other.
- **Plugin workspace note:** `plugins/` is a nested Cargo workspace excluded from the root
  `flux` workspace (so the root `cargo build --workspace` gate doesn't compile plugin deps);
  the plugin-side guard test and build must be run via `--manifest-path plugins/Cargo.toml`.
  `flux-codegate` scans only `crates/*`, so adding `schemars` to plugin crates is **not** a
  layering violation (plugins are L4 subprocess binaries, not crate-dep edges).
