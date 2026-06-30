---
id: D-33
title: Resolve cluster/namespace aliases in endpoint discovery
pillar: Agent
status: done
priority:
epic: session-s251-postmortem
design: docs/designs/session-s251-postmortem.md
note: "dev" isn't a kubeconfig context (it's a full EKS ARN); the broker never relays cluster/namespace; and "namespace=latest" is ambiguous with the newest-namespace heuristic
---

# Resolve cluster/namespace aliases in endpoint discovery

## Goal

Make `endpoint.discover` actually answerable: resolve short cluster aliases like `"dev"` to the
concrete kubeconfig context, relay structured `cluster`/`namespace` through the broker (not just
free-text `query`), and disambiguate `namespace=latest` (a literal name) from the "newest namespace"
heuristic. Today the agent has to recover by hand (list contexts → eyeball → hardcode the ARN), and
discovery against the *correct* cluster still returns `{"candidates": []}` because the wrong
namespace is searched. Serve the Agent pillar value: the discovery spine is a trustworthy entry
point, not a manual-recovery puzzle.

## Acceptance

- [x] **Failing-first test (alias)** — `cluster_alias_resolves_to_concrete_context` in
      `plugins/kubernetes/src/main.rs`. Fails on the old code (`cluster` ignored → discovery ran
      against the current/prod context → empty); passes after `resolve_context_alias` resolves
      `dev` → the `dev-eu` kubeconfig context.
- [x] **Failing-first test (latest ambiguity)** — `literal_latest_namespace_preferred_over_heuristic`.
      Fails on the old `query`-substring `wants_latest` heuristic (picked the newest namespace,
      `team-new`, which had nothing); passes after the substring heuristic is retired (only
      `latest_namespace: true` triggers the newest-namespace heuristic).
- [x] **Ambiguity is loud** — `resolve_context_alias` returns a clear error naming the candidate
      contexts on a >1 match, and naming the available contexts on a 0 match; never a silent empty.
- [x] **Broker relays structured fields** — `endpoint.discover` gains `cluster`/`namespace` fields;
      `parse_query_tokens` extracts `cluster=<x>`/`namespace=<y>` from free-text `query` (explicit
      params win, tokens stripped from the forwarded query); `ProviderInvoker::discover` +
      `EndpointBroker::discover`/`fan_out` thread them; `HostProviderInvoker` sends them in the
      provider payload. Test `broker_parses_cluster_and_namespace_tokens_from_query`.
- [x] **Op descriptions updated** — `endpoint.discover` and `kubernetes.endpoint.discover` specs
      document `cluster`/`namespace` and the `latest` interpretation (`latest_namespace: true` for
      the heuristic; a literal `latest` is `namespace: "latest"`).
- [x] Root gate green: `cargo test --workspace`, `clippy -D warnings` (workspace + plugins), `fmt`,
      `cargo test -p flux-codegate`, the `plugins/` gate, and both skill-sync tests.
- [x] CHANGELOG entry under `[Unreleased]`.

## Progress

- **Blocker resolved.** L-09 (named-argument calls) landed (`196dbdb`); D-33 promoted to
  `in-progress`. The structured form carries `cluster`/`namespace` as named fields throughout.
- **Provider (kubernetes plugin):**
  - Added `resolve_context_alias` — resolves a short `cluster` alias against kubeconfig context
    names (exact match wins; else case-insensitive substring; 0/>1 matches are loud errors). Called
    once at the top of `endpoint_discover`; the resolved concrete context is injected as `context`
    so every downstream `ctx_args` call targets the resolved cluster.
  - Added a `cluster` field to the `kubernetes.endpoint.discover` op spec + updated the description.
  - Retired the `query`-substring `wants_latest` heuristic (the s_251 ambiguity): `wants_latest` is
    now true **only** for `latest_namespace: true`. A literal namespace named `latest` is just
    `namespace: "latest"`. Updated `endpoint_discover_selects_latest_namespace` to use
    `latest_namespace: true`.
- **Broker (`flux-capabilities/src/endpoint/broker.rs`):**
  - `parse_query_tokens` extracts `cluster=`/`namespace=` from free-text `query` (stripped remainder
    forwarded; non-string query unchanged).
  - `ProviderInvoker::discover`, `EndpointBroker::discover`, `fan_out` gained `cluster`/`namespace`;
    `HostProviderInvoker` sends them in the provider payload; `refresh` passes `None`/`None`.
  - Updated all test fakes (`FakeInvoker`, `MutInvoker`, `OpRecordingInvoker`, `OnePg`,
    `OneCandidate`) + call sites to the new signature.
- **Agent-facing op (`ops.rs`):** `DiscoverOp` spec + execute read `cluster`/`namespace` and pass
  them to the broker; added a local `opt_str` helper.
- **Consumer-plugin path (`host_caps.rs`):** reads `cluster`/`namespace` from the plugin payload and
  forwards them to the broker.
- **Failing-first verified:** both provider tests fail on the pre-fix code (alias: `cluster`
  ignored; latest: substring heuristic) and pass after; the broker test fails on the pre-fix broker
  (no parsing → `cluster=None`) and passes after.
- **Gate:** workspace + plugins `cargo test` green (D-33 crates stable across repeated runs);
  `clippy -D warnings` clean (workspace + plugins); `fmt` clean; `flux-codegate` green;
  `skill_in_sync` + `skill_docs_in_sync` green (op-description changes are in `flux-capabilities`,
  not the generated language tables).
- **Note (out of scope):** `flux-config::tests::loads_project_config` is a pre-existing intermittent
  workspace-parallelism flake (latent temp-dir race in its test helper) — it passes consistently in
  isolation and is unrelated to D-33 (`flux-config` is untouched). Surfaced here for visibility; not
  addressed in this story.

## Notes

- Root cause + evidence in [docs/designs/session-s251-postmortem.md](../designs/session-s251-postmortem.md)
  §Defect 2. The model manually recovered via `kubernetes.cluster.list()` then hardcoded the ARN
  (`s_251` plan @seq 38); that manual step should become unnecessary.
- **Depends on the in-flight positional-args → kwargs work** in the kubernetes plugin: the structured
  form must carry `cluster` and `namespace` as **named fields**, not positional args. Coordinate with
  that session; if it hasn't landed, this story is `blocked` until it does.
- Implementation sites: `plugins/kubernetes/src/main.rs` — `ctx_args` (line ~486), `cluster_list`
  (~543), `resolve_namespaces` (~734), `wants_latest` (~752), `latest_namespace` (~769),
  `endpoint_discover` (~607), `discover_db_secrets` (~949); and
  `crates/flux-capabilities/src/endpoint/broker.rs:150` (the `{product, query, limit}` payload) +
  `crates/flux-capabilities/src/endpoint/ops.rs` (`DiscoverOp` spec).
- Discovery stays read-only and weak-ref-only (URLs + credential *references*, never values) — no
  change to the references-only IO invariant.
- Sibling story [L-08](L-08-ctx-pack-eviction.md) fixes the packer defect that compounded with this
  one; both are needed for the "check db connectivity" path to be trustworthy.
