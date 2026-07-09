---
id: D-115
title: Surface endpoint ops from the endpoints store — and gate all five together
pillar: Agent
status: done
design: docs/designs/datasource-discoverability.md
epic: datasource-discoverability
note: "the `endpoint` group surfaces ONLY on the ambient kubernetes signal (kubeconfig present); a postgres endpoint registered in ~/.flux/endpoints.toml never surfaces the ops — and endpoint.import is missing from the group manifest, so the one write-effect endpoint op is ALWAYS advertised while the four read ops are gated (inverted)"
---

# Surface endpoint ops from the endpoints store — and gate all five together

## Goal
An agent session in which endpoints exist should see the endpoint ops — kubeconfig or not. Inject
the already-honored-but-never-emitted `endpoint` signal when the persisted endpoint store is
non-empty, and fix the group manifest so all five endpoint ops gate together.

## Acceptance
- [x] Failing-first test: with a non-empty endpoints store and no `kubernetes` signal,
      `detect_signals` (or the session's signal assembly) includes `endpoint` and
      `resolve_active_groups` surfaces the endpoint group; with an empty/missing store and no
      kubeconfig it does not.
- [x] `endpoint.import` added to the `endpoint` group's tool list in
      `crates/flux-tools/src/groups.rs:106-111` (registered five vs listed four today) — test pins
      group manifest == `endpoint_tools()` names so they can't drift again.
- [x] The `endpoint.import` spec doc-comment/impl mismatch ("Not read-only" comment on a
      `ToolSpec::read_only` + `LocalSystem` effect, `endpoint/ops.rs:299-315`) resolved while there.
- [x] Signal detection stays cheap: prefer the startup-loaded `EndpointRegistry` over re-reading
      `~/.flux/endpoints.toml` per turn (see design doc risk note).

## Progress
- 2026-07-09 filed from the datasource-discoverability grounding pass (see design doc).
- 2026-07-09 — Implemented as a session-ambient-signal seam (the "session's signal assembly"
  option): `FlowEngine` gained a private `ambient_signals: Vec<String>` +
  `with_ambient_signals(..)` builder, appended to `detect_signals`' result inside
  `surfaced_op_names` (both call sites — the loop host's per-turn path and `flux plan`'s), so
  host-known facts gate groups identically to workspace-probed signals. `AgentSpec` gained the
  matching public `ambient_signals` field (threaded through `into_engine`; **SDK-visible literal
  constructors need the new field or `..Default::default()`** → minor bump per the semver rule).
  The CLI computes the signal ONCE at startup from the just-loaded registry (new
  `EndpointRegistry::is_empty()`; `session_ambient_signals` helper) — no per-turn re-read;
  sticky-monotonic surfacing makes startup-static sufficient, and the mid-session store writers
  (`endpoint.discover`/`import`) are themselves in the gated group. Manifest: `endpoint.import`
  added to the group's tool list; `ImportOp` spec rebuilt as a literal (LocalSystem effect,
  Risk::Low, Idempotent — the "Not read-only" comment now matches the construction); "four ops"
  doc comments corrected to five. Tests: flux-flow
  `ambient_signals_surface_groups_without_workspace_evidence` (plumbing), flux-cli
  `endpoint_group_manifest_matches_endpoint_tools` (drift-pin — red before the manifest fix) and
  `endpoint_store_signal_surfaces_group_without_kubeconfig` (store → signal → group with real
  `builtin_groups`). Live smoke: temp `HOME` + endpoints.toml + `KUBECONFIG=` → `groups.active`
  observation shows `endpoint` in groups AND signals; empty store → absent. Gate green
  (fmt/clippy/test workspace + codegate).

- 2026-07-09 — **Correction from the post-implementation review:** the filing-time "inverted
  gating" premise was wrong — `flux_runtime::effective_group` falls back to an op's own
  `ToolSpec::group` tag when no manifest group lists it, and `ImportOp` always carried
  `.with_group(ENDPOINT_GROUP)`, so `endpoint.import` was never actually advertised while its
  siblings were gated. The manifest addition + drift-pin test stand as *explicitness* (the
  manifest is what config reassignment edits), not a behavior fix; CHANGELOG re-worded
  accordingly. Review fixes also landed: the store's `load()` error is surfaced at startup
  (was silently swallowed — a corrupt endpoints.toml would have silently defeated this whole
  feature), the startup `project.signals` observation now records ambient signals, the CHANGELOG
  entry carries an SDK migration note for the new `AgentSpec` field (released as a patch — the
  operator's call on framing), and WHATS-NEW gained the customer entry.

## Notes
- Group + gate comment: `crates/flux-tools/src/groups.rs:100-115` ("a generic `endpoint` signal,
  if ever injected, surfaces it too" — this story injects it).
- Ambient-signal precedent: `kubernetes` / `shell` in `crates/flux-runtime/src/lib.rs:574-594`.
- Registry load + persistence: `crates/flux-capabilities/src/endpoint/mod.rs:67,122-165`;
  CLI wiring `crates/flux-cli/src/main.rs:2177-2180`.
