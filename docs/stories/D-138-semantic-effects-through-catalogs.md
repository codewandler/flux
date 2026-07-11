---
id: D-138
title: Surface semantic FlowEffects through OpSignature + the plugin-manifest OperationSpec
pillar: Language
status: done
epic:
design:
note: "downstream ask (ai-agent-platform flows arc, ask 4): OpSpec::lower() drops Money/Delete/SendExternal (Money vanishes, Delete→Write, SendExternal→Network) — no catalog the platform sees carries the semantic tier; authored bind/memo effect tags (D-133) are the interim"
---

# Surface semantic FlowEffects through OpSignature + the plugin-manifest OperationSpec

## Goal
A consumer reading an op catalog (the SDK `OpSignature`, or a plugin's manifest `OperationSpec`)
can see the op's SEMANTIC effect tier (`Money`/`Delete`/`SendExternal`), not just the host tier
(`read`/`write`/`network`). Today `OpSpec::lower()` erases it (`Money` vanishes entirely,
`Delete`→`Write`, `SendExternal`→`Network` — flux-lang `effects.rs`), so a downstream visual
editor (ai-agent-platform F-24) can only pin catalog-derived risk tiers; distinct Money/Delete
badges need the AUTHOR to tag each call site (`effect: money` on a `bind`, surfaced per node by
D-133's `annotate_effects`).

## Acceptance
- [x] `OpSignature` carries the op's declared semantic effects alongside the lowered host effects
      (additive — existing consumers unaffected).
- [x] The plugin manifest's `OperationSpec` can declare them, and the manifest→catalog adapter
      preserves them end-to-end.
- [x] `annotate_effects` (D-133) folds catalog-declared semantics into per-node annotations without
      requiring an authored `effect:` tag on the call site.
- [x] Failing-first test: an op declaring `Money` in its spec annotates a plain (untagged) call
      node with `Money`.

## Progress
- 2026-07-12 — filed from the ai-agent-platform flows arc (flows.md upstream ask 4). Downstream is
  unblocked for authored tags (D-133 shipped in 0.15.0); this lifts the per-call-site authoring
  burden to the catalog level.
- 2026-07-11 — Implemented end-to-end. `OpSignature` (`crates/flux-lang/src/opspec.rs`) gained a
  `semantic_effects: Vec<FlowEffect>` field; `OpSpec::to_signature()` derives it directly from an
  `OpSpec`'s own `effects` (preserving `Money`/`Delete`/`SendExternal` that `lower()`'s `ToolSpec`
  still can't carry — a `ToolSpec` stays free of any `flux-lang` dependency by design). The plugin
  manifest's `OperationSpec` (`crates/flux-plugin/src/lib.rs`) gained a typed
  `semantic_effects: Vec<FlowEffect>` field (flux-plugin now depends on flux-lang — L4→L0, layering-
  legal, verified by `cargo test -p flux-codegate`); `PluginTool` projects it onto a new
  `flux_runtime::Tool::semantic_effects(&self) -> Vec<String>` default-empty trait hook (plain tag
  strings so the safety-envelope's core trait stays flux-lang-free). `flux-flow`'s `OpRegistry`
  (`registry.rs`'s new `signature_for_tool`) parses those tags back via `FlowEffect::from_tag` onto
  `OpSignature::semantic_effects` — the manifest→catalog adapter acceptance #2 names. `analyze.rs`'s
  `call_effect_annotation` now folds `sig.semantic_effects` into a call's `EffectAnnotation` (D-133),
  no authored `effect:` tag required. Consolidated the three duplicate `FlowEffect` tag-string tables
  (`format.rs`/`render.rs`/`parse.rs`) onto new canonical `FlowEffect::tag`/`FlowEffect::from_tag`.
  Failing-first test `annotate_effects_folds_catalog_declared_semantics_without_an_authored_tag`
  (`analyze.rs`) — verified it fails ("got [Network]") before the `call_effect_annotation` fold-in,
  passes after. Added `opspec_to_signature_preserves_semantic_effects_lower_erases` (opspec.rs) and
  `operation_spec_semantic_effects_project_onto_tag_strings` (flux-plugin) as direct unit coverage of
  the two new seams. Updated `crates/flux-lang/docs/reference.md`'s `FlowEffect` prose (catalog-
  declared, not just authored). Gate green (scoped): `cargo build --workspace`; `cargo test` +
  `cargo clippy --all-targets -D warnings` + `cargo fmt --check` for
  `codewandler-flux-lang`/`codewandler-flux-flow`/`codewandler-flux-plugin`/`codewandler-flux-runtime`/
  `flux-lsp`; `cargo test -p flux-codegate` (layering); plugins/ nested workspace
  (`codewandler-flux-host-kit`, `kubernetes`) built/tested/clippy/fmt-checked too since their
  `OperationSpec` literals needed the new field. `cargo fmt --all` was never run (another agent has
  in-flight files in this tree) — only the touched packages were formatted.
