---
id: A-41
title: "Role `model:` override goes through provider-spec parsing, not verbatim to the wire"
pillar: Agent
status: done
note: "hit live 2026-07-06 running examples/god-review.flux: a role with `model: openrouter/deepseek/…` 400s mid-turn with a raw provider error — the frontmatter string is sent to the wire verbatim while `-m` accepts the same spec form"
---

# Role `model:` override goes through provider-spec parsing, not verbatim to the wire

## Goal
A sub-agent role's `model:` frontmatter (`.flux/agents/<role>.md`) flows verbatim into
`AgentSpec.model` (`Role::to_spec`, applied at `crates/flux-orchestrate/src/lib.rs:310`) and from
there straight onto the **parent's** provider wire — sub-agents inherit the parent provider by
design. But `-m` accepts provider-prefixed specs (`openrouter/deepseek/deepseek-v4-flash`), so
users naturally write the same form in role frontmatter and get an opaque HTTP 400 from the
provider mid-flow ("… is not a valid model ID"). The role model override should speak the same
spec language as `-m`: resolve what can be resolved, and fail fast with a diagnostic that names
the actual constraint for what can't.

## Acceptance
- [x] A role `model:` value naming the **parent's own provider** as prefix (e.g. role says
      `openrouter/deepseek/deepseek-v4-flash` while the parent runs on `openrouter`) is accepted:
      the prefix is stripped and the provider-local slug goes to the wire. Failing-first test.
- [x] A role `model:` value naming a **different** provider fails fast at role-load/spawn time with
      a clear diagnostic stating that sub-agents inherit the parent provider (naming both
      providers) — not a raw wire error mid-turn. Failing-first test.
- [x] Bare provider-local slugs (the current working form) behave exactly as before.
- [x] Prefix matching must not naively split on `/` — openrouter model ids legitimately contain
      slashes (`vendor/model`). Match against known provider names only (reuse the existing spec
      parsing/canonicalization seam, e.g. what `-m` routing and `flux_core::canonical_model_spec`
      already use — do not invent a second parser).
- [x] Docs: the sub-agent role reference (AGENTS.md "Add a sub-agent role" and/or docs/usage.md)
      states the accepted `model:` forms and the inherit-parent-provider constraint.

## Progress
- 2026-07-06 filed — discovered while wiring `.flux/agents/god-reviewer.md`; workaround documented
  in that file (provider-local slug + comment). Validation notes in `review.md`.
- 2026-07-06 implemented. Added `flux_core::resolve_role_model(parent_provider, role_model)`
  (`crates/flux-core/src/pricing.rs:171` region, exported via `crates/flux-core/src/lib.rs:25`) —
  reuses the existing private `split_provider`/`known_provider` seam `canonical_model_spec` already
  uses (no second parser). Behavior: exact-string-prefix strip when `role_model` starts with
  `"{parent_provider}/"`; else if the leading segment is a *different* known provider, returns
  `flux_core::Error::Config` naming both providers; else (bare slug, or an unrecognised leading
  segment) passes through unchanged. Wired at the one production call site,
  `crates/flux-orchestrate/src/lib.rs` `LocalSpawner::spawn`, right after `role.to_spec(...)`: when
  `role.model` is `Some(_)`, `spec.model` is recomputed via `resolve_role_model(&provider_name, ..)`
  with `?` propagating the config error (role.model == `None` is untouched — it already inherits the
  parent's already-correct `default_model`). `flux_agent::Role::to_spec` itself is unchanged (it has
  no provider context; the resolution needs the provider name, which is only available at the
  spawn call site — see `provider_name` captured at `lib.rs:241`).
  - Failing-first tests (confirmed failing before the `spawn()` wiring, via a
    `ModelCapturingProvider` mock that records the `Request.model` actually sent to the wire):
    `flux-orchestrate::tests::spawn_strips_role_model_prefix_matching_parent_provider`,
    `flux-orchestrate::tests::spawn_rejects_role_model_naming_a_different_provider`.
  - Pure-function unit tests in `flux-core`:
    `pricing::tests::resolve_role_model_strips_matching_parent_provider_prefix`,
    `pricing::tests::resolve_role_model_rejects_a_different_known_provider`,
    `pricing::tests::resolve_role_model_distinguishes_openrouter_variants` (proves `openrouter` vs
    `openrouter-anthropic` are never treated as prefixes of one another),
    `pricing::tests::resolve_role_model_passes_bare_and_unknown_prefixed_ids_unchanged`.
  - Existing `flux-agent::role::tests::to_spec_inherits_model_and_carries_tools` (and the rest of the
    `role.rs` suite) untouched and still green — bare slugs behave exactly as before.
  - Docs: added a "Sub-agent role `model:` overrides" paragraph to `docs/usage.md` (under "Models &
    providers") stating the accepted forms and the inherit-parent-provider constraint. `AGENTS.md`
    is out of my edit boundary for this story; proposed one-sentence addition to its "Add a
    sub-agent role" bullet (line 120), for the orchestrator to apply:
    > `model:` accepts the same spec form as `-m` (bare id, or `provider/model`), but the leading
    > provider must be the parent's own — sub-agents inherit the parent's provider, so a different
    > provider prefix fails fast at spawn time instead of reaching the wire
    > (`flux_core::resolve_role_model`).
  - Gate (package-scoped): `cargo build -p flux-agent -p flux-orchestrate -p flux-core` clean;
    `cargo test -p flux-agent -p flux-orchestrate -p flux-core` — 10 + 24 + 27 passed, 0 failed;
    `cargo clippy -p flux-agent -p flux-orchestrate -p flux-core --all-targets -- -D warnings`
    clean; `cargo fmt -p flux-agent -p flux-orchestrate -p flux-core -- --check` clean (no diff);
    `cargo test -p flux-codegate` (layering lint) green.

## Notes
- Discovery context: god-review flow run, session s_410; first attempt failed with
  `HTTP 400: openrouter/deepseek/deepseek-v4-flash is not a valid model ID`.
- Cross-provider role overrides (actually building a second provider for the child) are explicitly
  OUT of scope — that would need a provider-factory seam below L6 (`build_provider` lives in
  flux-cli) and a credentials story. This story is about spec parsing + honest errors.
