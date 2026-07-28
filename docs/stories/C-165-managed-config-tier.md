---
id: C-165
title: Managed config tier — an enforced baseline a local user cannot override
pillar: Core
status: done
epic:
design:
note: "config is a two-layer user→project merge (flux-config lib.rs:968-973) where BOTH layers are writable by the same user, so there is no way to pin a policy floor an operator can't edit — the landscape doc names regulated/auditable buyers as flux's whitespace, and this is the missing half of that story; needs no backend"
---

# Managed config tier — an enforced baseline a local user cannot override

## Goal
Let an organization pin a floor. Today `load()` merges exactly two layers — the user's home config
and the project's `.flux/config.toml` (`crates/flux-config/src/lib.rs:968-973`) — and both are
writable by the person running flux, so every setting is advisory. A **managed** layer (a
system-owned path, or one pinned by an environment channel) that takes precedence over both, and
whose security-relevant keys cannot be relaxed downstream, turns flux's default-deny envelope from
"the default a developer accepted" into "the baseline an auditor set".

## Acceptance
- [ ] A third config layer loads ahead of user and project, from a documented system location
      (plus an explicit override channel for containerized deploys) — failing-first test asserting
      precedence over both existing layers.
- [ ] The layer distinguishes **defaults** (a starting value the user may change) from **pins** (a
      value the user may not change). A downstream layer attempting to relax a pinned
      security-relevant key is refused with a named diagnostic, not silently ignored — test covers
      both a permitted override and a refused one.
- [ ] Relaxation is refused in the *permissive* direction only: a project may still make itself
      **more** restrictive than the managed baseline. Pinned by test in both directions.
- [ ] The effective configuration is inspectable — one command shows each setting's value and which
      layer it came from, so "why can't I enable this" has an answer (natural home: the C-128
      `flux doctor` diagnostics if that lands first).
- [ ] The managed file's own trust is stated honestly in the docs: this is an **operator** control
      backed by filesystem permissions, not a defense against a user who owns the machine and can
      edit the binary. Overclaiming here would be worse than not shipping it.
- [ ] Website security docs updated truthfully (the C-16 / L-19 / D-137 docs-truth pattern).

## Progress
- (not started — filed from the 2026-07-28 Amp feature-mining pass, second pass)
- 2026-07-28: Implemented, uncommitted. Summary (see final report for full detail):
  - `flux-config`: `PinnableKey` (7 keys: `tools.disable`, `private_net.web`, `policy`,
    `sandbox.{enabled,require,network}`, `workspace.allow_all`), `ManagedMeta`/`[managed] pins`,
    `ConfigLayer`, `EffectiveSetting` + `effective_settings()`, `pin_violation()` +
    `grant_more_permissive()`, and `from_sources_with_managed()` (managed→user→project fold,
    pin-checked against `merge(user, project)` before folding). `from_sources` untouched.
  - `flux-runtime::metadata`: `load_config` now reads a third guarded source — `FLUX_MANAGED_CONFIG`
    (exact file, wins outright) else `/etc/flux/config.toml` on Linux/macOS else none — and calls
    `from_sources_with_managed`. New `config_layers()` returns the three raw (unmerged) layers for
    provenance reporting.
  - `flux-cli::doctor`: new "config provenance (C-165)" check (`judge_config_provenance` /
    `check_config_provenance`), filling the seam the C-128 author had already left in `CHECKS` for
    this. Always PASS (inspection, not judgment) — a real pin violation is a hard `load_config`
    error surfaced before `doctor` runs at all. `DoctorCtx` gained `managed_cfg`/`user_cfg`/
    `project_cfg`; all 7 construction sites updated.
  - Docs: `website/docs/reference/config.md` new "Managed configuration tier (operator floor)"
    section (precedence line updated, worked example, pin-vs-default explanation, honest-trust
    paragraph, `FLUX_MANAGED_CONFIG` in env overrides, Related-docs cross-refs);
    `website/docs/security/overview.md` "An honest posture" gained a matching bullet.
  - Tests added this session (flux-runtime, all green): `load_config_reads_flux_managed_config_env_override`,
    `load_config_managed_layer_precedes_user_and_project_for_defaults`,
    `load_config_surfaces_a_pin_violation_as_a_named_error`,
    `load_config_without_any_managed_source_is_unaffected`, `config_layers_returns_each_raw_unmerged_layer`.
    Tests in flux-config from an earlier pass this session (all green): `managed_layer_default_wins_when_unset_downstream_but_loses_to_an_explicit_override`,
    `from_sources_without_managed_layer_is_unchanged`, `pinned_private_net_web_refuses_widening_but_permits_narrowing_or_silence`,
    `pinned_policy_refuses_any_additional_downstream_grant`, `pinned_tools_disable_permits_additional_downstream_entries_in_both_cases`,
    `unrecognized_pin_name_is_a_load_time_error`, `effective_settings_reports_layer_and_pin_status`,
    `pinnable_key_parse_and_as_str_round_trip`. Tests added in `flux-cli/src/doctor.rs` (written,
    NOT run — see blocker below): `judge_config_provenance_passes_and_names_every_key_when_nothing_is_managed`,
    `judge_config_provenance_names_the_pinned_key_and_its_managed_value`,
    `check_config_provenance_attributes_a_project_only_setting_to_the_project_layer`.
  - Gate — all green, full detail in the final report: `cargo test -p codewandler-flux-config -p
    codewandler-flux-runtime` (128 passed); `cargo test -p flux-cli --bin flux` (213 passed,
    including the 3 new `doctor::tests::*config_provenance*`) and `cargo test -p flux-cli --test
    website_contract` (13 passed, including `public_config_examples_deserialize_and_have_effect`
    against the new managed-tier TOML example); `cargo clippy -p codewandler-flux-config -p
    codewandler-flux-runtime -p flux-cli --all-targets -- -D warnings` clean; `rustfmt --check`
    clean on all four edited Rust files.
  - Transient blocker (now resolved): `cargo build -p flux-cli` initially failed on an unrelated,
    concurrent, uncommitted change — `crates/flux-flow/src/lib.rs` declared `pub mod wakeup;` with
    no backing `crates/flux-flow/src/wakeup.rs` yet (another session's in-flight A-98 story,
    mid-write). Did not touch that file; waited and retried, and it resolved once that session
    finished writing it — full flux-cli gate above is post-resolution and green.
  - Checkboxes left unchecked and `status` left `in-progress` per instruction, despite the gate
    being green — Acceptance items 1-3, 5, 6 and the `flux doctor` half of item 4 are implemented
    and test-covered; nothing known is missing. Left for the closing session's own judgment call
    rather than self-closing here.

## Notes
- Source: [../research/amp.md](../research/amp.md) — Amp's Enterprise "managed settings
  (system-wide enforcement)". **This story exists because the first mining pass got it wrong**: it
  was bulk-rejected under "enterprise features need a hosted control plane." Managed settings need
  no backend at all — they are a file and a precedence rule.
- Evidence the gap is real: `crates/flux-config/src/lib.rs:968-973` — `load()` is
  `merge(user, project)`, full stop.
- Strategic weight: [`../archive/research/landscape.md`](../archive/research/landscape.md) Part 2
  argues flux's open lane is local-first + auditable + default-deny for regulated buyers. A policy
  floor a developer cannot silently lower is the missing half of that claim — today an auditor has
  to trust that nobody edited `.flux/config.toml`.
- Interacts with the authorization policy and the sandbox config (D-134: `require` mode) — decide
  deliberately which keys are pinnable, and keep that list small and security-relevant rather than
  making everything pinnable because it is easy.
