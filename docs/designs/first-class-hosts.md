# Design — First-class hosts

## Why

The substrate abstraction already exists — `flux-system` owns the guarded port, `flux-codegate`
pins the backend census, and the remote protocol ships — but the *entity* does not. The remote
substrate is an anonymous `--remote <url>` flag, the Exchange binding is a transitional
environment-variable pair, sandboxing is a spawn-time modifier rather than a selectable backend,
and HTTP cannot follow a selected substrate at all. Decision 0018 names the missing thing: a Host
is a named, first-class binding to an execution substrate, granted per principal or layer — the
question is always *who may use a binding*, never merely whether an address is reachable.

## Approach

Reuse the endpoint entity pattern wholesale: a `[[host]]` config table, a session registry, a
`flux host` CLI family and an ambient-gated `host.*` operation group. The guarded port stays the
only IO seam; backends become peers (local, sandboxed, container, kubernetes, microvm, remote) that each
pass the codegate census under review; `GuardedHttp` joins the port so `flux-web` placement can
move to `SelectedExecutionSystem`; and middleware stays at the dispatch layer, where gating,
telemetry, redaction and substrate provenance already flow through one choke point.

## Stories

- C-648 — a host config table, `HostRef` and session registry
- C-649 — `flux host` CLI family and `host.*` operations
- C-650 — a named host binding selects the execution substrate
- C-651 — sandboxed is a selectable peer backend
- C-652 — HTTP joins the guarded port
- C-677 — a microvm binding resolves to a served guest substrate: the `microvm` word over the
  delivered remote client, unwired and fail-closed until it names an endpoint C-480's guest
  profile serves. No provisioning verb; no new wire.
- Context members delivered under earlier contracts: C-397 (container process backend), C-480
  (OCI image, manifests, microVM unit)
