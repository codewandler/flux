---
id: D-115
title: Surface endpoint ops from the endpoints store — and gate all five together
pillar: Agent
status: ready
priority: 21
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
- [ ] Failing-first test: with a non-empty endpoints store and no `kubernetes` signal,
      `detect_signals` (or the session's signal assembly) includes `endpoint` and
      `resolve_active_groups` surfaces the endpoint group; with an empty/missing store and no
      kubeconfig it does not.
- [ ] `endpoint.import` added to the `endpoint` group's tool list in
      `crates/flux-tools/src/groups.rs:106-111` (registered five vs listed four today) — test pins
      group manifest == `endpoint_tools()` names so they can't drift again.
- [ ] The `endpoint.import` spec doc-comment/impl mismatch ("Not read-only" comment on a
      `ToolSpec::read_only` + `LocalSystem` effect, `endpoint/ops.rs:299-315`) resolved while there.
- [ ] Signal detection stays cheap: prefer the startup-loaded `EndpointRegistry` over re-reading
      `~/.flux/endpoints.toml` per turn (see design doc risk note).

## Progress
- 2026-07-09 filed from the datasource-discoverability grounding pass (see design doc).

## Notes
- Group + gate comment: `crates/flux-tools/src/groups.rs:100-115` ("a generic `endpoint` signal,
  if ever injected, surfaces it too" — this story injects it).
- Ambient-signal precedent: `kubernetes` / `shell` in `crates/flux-runtime/src/lib.rs:574-594`.
- Registry load + persistence: `crates/flux-capabilities/src/endpoint/mod.rs:67,122-165`;
  CLI wiring `crates/flux-cli/src/main.rs:2177-2180`.
