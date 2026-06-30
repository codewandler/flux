---
id: D-27
title: Reference-based IO & host-injected connect (the invariant, enforced)
pillar: Core
status: done
design: docs/designs/endpoint-discovery.md
note: "enforces the references-only invariant: `http.do`/`conn.dial` take an `endpoint_ref`, the host resolves + injects the credential (cross-plugin `Kubernetes`-scheme via the owning plugin's `secret.read`), gated deny-by-default + operator grant + first-use-approval seam + `CrossPluginResolve` audit; the gated `credential` capability serves raw-socket in-band auth (trusted plugin only, never the model)"
---

# Reference-based IO & host-injected connect (the invariant, enforced)

## Goal
Cut the host IO capabilities (`http` / `conn`) over to taking an **`endpoint_ref` + a relative
path / sub-target** instead of a plugin-assembled URL + env-named auth. The host composes the absolute
URL and injects the credential host-side — resolving cross-plugin `Kubernetes`-scheme references via
the provider's gated `secret.read` — so the consuming plugin and the model **never see a URL with
credentials or a raw secret**. This is the story that *enforces and tests* the reference invariant,
and gates cross-plugin credential use deny-by-default + grant + first-use approval + audit.

## Why
[D-25](D-25-endpoint-reference-model.md) introduces references and [D-26](D-26-endpoint-discovery-broker.md)
produces them, but IO still flows the old way: `http.do` takes a plugin-built URL + `auth_purpose`, and
`conn.dial` a `tcp:host:port` the plugin assembled from a resolved URL string. Until IO is ref-based,
the invariant ("a plugin op deals only in references") is not actually enforced. Cross-plugin
credentials (k8s discovers an RDS secret, `sql` connects) widen the trust surface, so resolution is
gated, not implicit. See the [epic design](../designs/endpoint-discovery.md) — *Ref-based IO & connect*
and *Security model*.

## Acceptance
- [x] **Ref-based IO** — `http.do`/`conn.dial` accept an `endpoint_ref` (+ relative `path`); the host
      resolves it via the injected `ReferenceResolver`, composes the absolute URL through the existing
      egress guard, and injects the credential host-side. The plugin frame carries only the ref. Tests
      `http_by_ref_injects_host_side`, and the consumer-gated `resolve_endpoint_for`.
- [x] **Cross-plugin credential** — a `Kubernetes`/`Plugin`-scheme `credential_ref` is materialized via
      the owning plugin's `secret.read` op (the `CredentialReader` seam), reusing the broker's
      re-entrancy guard. HTTP path: injected host-side (plugin never sees it). Raw-socket path: delivered
      to the trusted plugin via the gated `credential` capability (`PluginCapabilities.credential`,
      deny-by-default) for in-band auth — registered with the `Redactor`, never to the model. Tests
      `raw_socket_credential_gated_to_plugin_not_model`, `resolve_endpoint_materializes_credential_ref_into_bearer`.
- [x] **Deny-by-default cross-plugin gate** — no `[endpoint] cross_plugin_credentials` grant for the
      `(consumer:provider)` pair ⇒ refused, on BOTH the raw-socket and the HTTP-injection paths
      (consumer = the real calling plugin, not the record owner). Tests
      `cross_plugin_resolution_denied_without_grant`, `http_ref_with_cross_plugin_credential_denied_without_grant`.
      The consumer-agnostic `resolve_credential`/`resolve_endpoint` refuse cross-plugin injection so the
      gate can't be bypassed.
- [x] **First-use approval + audit** — a `CrossPluginApprover` seam (first use per `(consumer,provider)`,
      session-cached; no approver ⇒ config grant alone authorizes headless) + a `flux-events`
      `CrossPluginResolve { consumer, provider, reference_location }` audit (location only, never the
      value). Tests `cross_plugin_first_use_approval_and_audit`, `cross_plugin_denied_when_approver_refuses`.
- [x] **Inline-credential URL splitting** — a `scheme://user:pass@host` URL is split (userinfo → injected
      `Basic` header, bare URL surfaced). Test `resolve_endpoint_strips_inline_credential_into_header`.
- [x] **Private-net via D-20** — ref-based IO runs through the existing scoped egress guard (D-20),
      never the global `allow_private_net`.
- [x] Gate green: `cargo test -p flux-secret -p flux-plugin -p flux-capabilities -p flux-config`, clippy
      `-D warnings` (+ flux-cli/flux-app), fmt, `flux-codegate`, full workspace build.

## Progress
- **Done.** Landed `SystemHostCaps.with_resolver`/`with_secret_sink` + ref-based `http.do`/`conn.dial`
  + the gated `credential` capability; `EndpointBroker` now implements `ReferenceResolver`
  (`resolve_endpoint`/`resolve_endpoint_for`/`resolve_credential`/`resolve_credential_for` +
  `credential_ref_for_endpoint`) with the cross-plugin gate (grant → approver seam → audit), the
  `CredentialReader` seam (provider `secret.read`), and inline-credential URL splitting. `Redactor` made
  clone-sharing (`Arc<Mutex>`, `add_secret(&self)`) so a mid-run materialized credential is scrubbed by
  the executor's clone. Config: `EndpointConfig.cross_plugin_credentials`. Wired into `flux run`
  (`build_agent`) fully; `flux app run` (`run_app`) wires the resolver + grants with `TODO(D-27)` for the
  secret-sink/audit (no EventStore/redactor in scope there, mirroring the D-20 gap) + the interactive
  approver.

## Notes
- **Honors AGENTS.md "no bypass paths"** — resolution + IO stay behind `Executor::dispatch`; this makes
  the envelope finer-grained, it does not skip it. Mirrors D-20's framing.
- Reuse: `resolve_auth`/`resolve_purpose` injection (`crates/flux-plugin/src/lib.rs:586–616`); the
  D-20 scoped allow-set; the `flux-events` audit substrate (D-02).
- Blocks [D-28](D-28-kubernetes-endpoint-provider.md), [D-29](D-29-migrate-plugins-to-references.md).
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).
