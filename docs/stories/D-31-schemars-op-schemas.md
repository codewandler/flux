---
id: D-31
title: schemars-derive every operation schema (kill hand-written JSON Schema)
pillar: Core
status: ready
priority:
design:
---

# schemars-derive every operation schema

## Goal

No in-process `ToolSpec` operation may hand-write its JSON Schema: every op's `input_schema`
must be derived from a typed Rust struct via `schemars` (`flux_spec::tool_input_schema::<T>()`),
so the schema and the runtime parsing cannot drift. This is the engineering work the agent in
session `s_249` failed to deliver (separately root-caused: the empty-parallel-branch bug, fixed
in `453379e`).

Unblocked by **L-09** (named-argument calls): `x-param-order` is gone and `required` is a set,
so `schemars`-derived schemas (which don't emit `x-param-order`) bind correctly.

## Acceptance

- [ ] `rg "input_schema:\s*(serde_json::)?json!\(\{" crates -g '*.rs'` returns **no** op-declaration
  hits (only test asserts remain) across `flux-tools`, `flux-eval`, `flux-orchestrate`.
- [ ] Every migrated op's schema comes from `tool_input_schema::<T>()`; handlers either parse via
  `parse_params` (full SSoT) or stay ad-hoc with a documented `#[allow(dead_code)]` struct
  (schema-only, matching the flux-eval precedent).
- [ ] `op.register`'s schema↔handler drift is resolved (one `JsonSchema` definition shared by
  schema + runtime, or documented deferral).
- [ ] A **regression guard** test (`crates/flux-tools/tests/no_manual_schema.rs`) fails on a
  deliberately reintroduced `input_schema: json!({...})` and passes on the migrated tree.
- [ ] A **drift report** (`DRIFT.md`) records every schema↔handler mismatch found + fixed.
- [ ] Dev loop green: `cargo build/test --workspace`, `clippy -D warnings`, `fmt`, `flux-codegate`.

## Scope

In-process `ToolSpec` ops only (~36 sites across `flux-tools`, `flux-eval`, `flux-orchestrate`).
The **275 plugin `OperationSpec` ops** are deferred — tracked separately (host-kit needs
`read_op_typed`/`write_op_typed` helpers; each plugin crate needs `schemars`).

## Progress

Work is in progress in the worktree `/home/timo/projects/flux-schemars-refactor`
(branch `refactor/schemars-schemas`). Foundations + ~half the files are migrated:
- `flux-orchestrate` (`task`): done (full SSoT).
- `flux-eval`: schemas migrated (SSoT deliberately deferred — coercion convention).
- `flux-tools`: `cargo`/`cognition`/`toolchains`/`extra`/`evidence` schemas migrated (schema-only);
  `reflect` schemas migrated (SSoT drift pending); `lib.rs` core (read/edit/bash/git_*) pending.
- Regression guard + drift report: not started.

Resume by: finishing `flux-tools/src/lib.rs`, fixing the `reflect.rs` SSoT drift, adding the
regression guard, writing `DRIFT.md`, then gating.

## Notes

- Pattern + precedent established: see `WriteInput`/`WriteTool` (full SSoT) and the flux-eval
  structs (`#[allow(dead_code)]` schema-only).
- Out of scope: `flux-lang/opspec.rs` (the composite-op schema *generator*), provider `json!`
  test/MCP-passthrough sites.
