---
id: D-33
title: Resolve cluster/namespace aliases in endpoint discovery
pillar: Agent
status: backlog
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

- [ ] **Failing-first test (alias)** — `endpoint.discover` / `kubernetes.endpoint.discover` with
      `cluster="dev"` resolves to the dev kubeconfig context (the ARN containing `/dev-`) and runs
      against it, instead of passing `"dev"` literally to `kubectl --context` and failing. Name the
      test `cluster_alias_resolves_to_concrete_context` (in `plugins/kubernetes`).
- [ ] **Failing-first test (latest ambiguity)** — against a cluster that has a literal namespace
      named `latest`, `namespace="latest"` uses it (not the newest-namespace heuristic). Name the
      test `literal_latest_namespace_preferred_over_heuristic`.
- [ ] **Ambiguity is loud** — an alias matching >1 context returns a clear error naming the
      candidates, never a silent empty result.
- [ ] **Broker relays structured fields** — `endpoint.discover` gains optional structured `cluster`
      and `namespace` fields; the broker parses `cluster=<x>` / `namespace=<y>` tokens out of
      free-text `query` into the structured fields and forwards them to providers. The literal-name
      and context-resolution paths are reachable through the broker without the agent hand-parsing.
- [ ] **Op descriptions updated** — the `endpoint.discover` and `kubernetes.endpoint.discover` specs
      document the `cluster`/`namespace` fields and the `latest` interpretation.
- [ ] Root gate green: `cargo test --workspace`, `clippy -D warnings`, `fmt`; the `plugins/` gate if
      the kubernetes plugin changed; a MockHost test per changed op.
- [ ] CHANGELOG entry under `[Unreleased]`.

## Progress

- (running log — a resuming agent reads this to know where things stand)

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
