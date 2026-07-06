---
id: D-57
title: flux-a2a schema derives (feature-gated utoipa) + shared card_url helper
pillar: Agent
status: done
epic: consumer-gaps
note: "from the 2026-07-06 downstream-consumer review: no schema derives on the a2a wire types forces consumers to hand-mirror them (10 structs + a drift test) for OpenAPI docs; card-URL derivation is duplicated consumer-side and in flux-server"
---

# flux-a2a schema derives + card_url

## Goal
Let a consumer generate OpenAPI/JSON-Schema docs over the A2A surface straight from flux's wire
types (feature-gated `utoipa::ToSchema` derives), and give the card-URL-from-request derivation one
shared home.

## Why (evidence)
- `flux-a2a`'s wire types (`crates/flux-a2a/src/types.rs`: `Message`:90, `TaskStatus`:173,
  `Task`:208, `AgentCard`:405, …) derive only serde. The reviewed downstream consumer maintains 10
  hand-written `#[derive(ToSchema)]` mirror structs plus a drift test in its OpenAPI module, and
  flags the fix in its own code: "Retire the mirrors once `flux_a2a` derives `ToSchema`".
- `flux-server/src/a2a.rs:138-147` inlines a scheme/host→URL derivation identical to the consumer's own
  host-URL helper. A pure `flux_a2a::server::card_url(...)` dedupes both.

## Acceptance
- [x] `utoipa` optional dependency (v5 — matches the consumer's workspace pin) + `utoipa` feature on
      flux-a2a, off by default; `#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]` on the
      wire types a consumer documents: the mirrored A2A protocol types (Message, Part
      and its variants, Task, TaskStatus/TaskState, AgentCard, AgentSkill, and the JSON-RPC
      request/response envelope types) — the consumer's mirror set defines the exact list and
      shape parity.
- [x] Test compiled under the feature proving the derives work (e.g. generate a schema for AgentCard
      and assert a known field appears); gate runs flux-a2a tests once with `--features utoipa`.
- [x] Pure `card_url` helper in flux-a2a's server module (framework-free: takes scheme/host — or
      header values — + path, returns the URL string, honoring `x-forwarded-proto` semantics);
      flux-server's inlined derivation switches to it (behavior-identical, existing tests pin it).
- [x] Full gate green; consumer-compat `cargo check` in the downstream consumer workspace unaffected (additive; feature off
      by default).

## Progress
- 2026-07-06 filed from the consumer review.
- 2026-07-07 implemented. `utoipa = { version = "5", optional = true }` declared **locally** in
  `crates/flux-a2a/Cargo.toml` (not `[workspace.dependencies]` — flux's convention for single-crate
  optional deps is local declaration, e.g. flux-capabilities' `ureq`/`fastembed`/`sqlite-vec`), gated
  by a same-named `utoipa = ["dep:utoipa"]` feature, off by default. Added
  `#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]` to every wire type in
  `crates/flux-a2a/src/types.rs` (see Notes for the full list) — not just the 10 the consumer's mirror
  covers, because utoipa's derive requires `ToSchema` on every field type reachable from a root
  (`Task.artifacts: Vec<Artifact>`, `AgentCard.capabilities: Capabilities`, etc. all needed it too).
  `serde_json::Map<String, Value>` flatten fields (`Part::extra`, `Artifact::extra`,
  `Capabilities::extra`) needed no `#[schema(value_type = ...)]` override — utoipa-gen's
  `#[serde(flatten)]` + map-type handling and its built-in `ToSchema for serde_json::Value` compose
  automatically into an `additionalProperties` schema. Added a feature-gated `schema_tests` module
  in `types.rs` (4 tests: `AgentCard`/`Message` field-name parity incl. `rename_all = "camelCase"`,
  `Role`/`TaskState` enum-value shape incl. the `#[serde(other)]` `Unknown` fallback rendering as its
  own value).
  Added `flux_a2a::server::card_url(forwarded_proto: Option<&str>, host: &str, path: &str) -> String`
  next to `agent_card` in `crates/flux-a2a/src/server.rs` (pure, no HTTP-framework types) with 3 unit
  tests, and switched `flux-server/src/a2a.rs`'s `agent_card` handler to call it instead of inlining
  the `format!("{scheme}://{host}/a2a")` derivation — same default-`"http"`/`"localhost"` fallback
  behavior, existing flux-server tests (none exercised this handler directly) stayed green unchanged.
  Gate: `cargo build --workspace`, `cargo test --workspace` (all crates green), `cargo test -p flux-a2a
  --features utoipa` (29 passed, was 26), `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo clippy -p flux-a2a --features utoipa --all-targets -- -D warnings`, `cargo fmt --check` (root
  + `plugins/`) all green. Consumer-compat: `cargo check --workspace` in the
  downstream consumer workspace is clean (additive; feature off by default) — nothing there was
  edited.

## Notes
- Adoption story filed in the consumer's own repo: delete the mirror structs + drift test, enable
  the feature, derive OpenAPI from flux types; switch its host-URL helper to card_url.
- Exact derive-target type list (all in `crates/flux-a2a/src/types.rs`), grouped by why each is
  there:
  - **Directly mirrored by the consumer**: `Message`, `Part`, `SendMessageParams`, `TaskStatus`,
    `Task`, `JsonRpcError`, `JsonRpcResponse<T>`, `JsonRpcRequest<P>`, `Skill`, `AgentCard`.
  - **Transitively required** (a field of one of the above; utoipa needs `ToSchema` on every
    reachable field type, and the mirror simplified these away): `Role` (`Message.role`), `TaskState`
    (`TaskStatus.state` — the mirror used a bare `String`), `Artifact` (`Task.artifacts`),
    `Capabilities` (`AgentCard.capabilities`), `AgentInterface` (`AgentCard.interfaces`),
    `SendConfiguration` (`SendMessageParams.configuration`).
  - **Not mirrored, added for completeness** (same module, same derive, no extra cost once the
    above compile): `TaskStatusUpdateEvent`, `TaskArtifactUpdateEvent`, `TaskGetParams`.
  - **Deliberately excluded**: `StreamEvent`, `SendOutcome` — hand-rolled client-side dispatch enums
    with no serde derive of their own (not wire types; they wrap an already-decoded `Task`/`Message`).
