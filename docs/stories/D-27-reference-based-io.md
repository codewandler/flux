---
id: D-27
title: Reference-based IO & host-injected connect (the invariant, enforced)
pillar: Core
status: backlog
priority:
design: docs/designs/endpoint-discovery.md
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
- [ ] **Ref-based IO** — `http`/`conn` host caps accept an `endpoint_ref` (+ relative path); the host
      resolves the ref → absolute URL → injects the credential (reusing `resolve_auth`'s
      Bearer/Basic/Header/Query), then performs the call. The plugin frame carries only the ref — never
      a URL or token. Failing-first test `endpoint::http_by_ref_injects_host_side`.
- [ ] **Cross-plugin credential injection** — a `credential_ref` with the `Kubernetes` scheme is
      materialized via the kubernetes plugin's gated `secret.read`; the consuming plugin never receives
      the value. Test `endpoint::cross_plugin_kubernetes_credential_injected`.
- [ ] **Deny-by-default** — cross-plugin resolution with no operator config grant for the
      *(consumer-plugin × product/endpoint)* is refused. Test
      `endpoint::cross_plugin_resolution_denied_without_grant`.
- [ ] **First-use approval + audit** — the first granted cross-plugin resolution prompts the approval
      gate (session-scoped thereafter) and emits a `flux-events` audit record naming consumer, endpoint,
      and grant. Test `endpoint::cross_plugin_first_use_approval_and_audit`.
- [ ] **Inline-credential URL splitting** — a URL carrying embedded credentials is split (cred →
      `credential_ref`) and the credential never appears in a logged/surfaced URL. Test
      `endpoint::inline_cred_url_split_and_redacted`.
- [ ] **Private-net via D-20** — discovered private/in-cluster hosts are reachable only under the
      [D-20](D-20-scoped-private-net-egress.md) scoped allow, never the global `allow_private_net`.
- [ ] Gate green: `cargo test -p flux-plugin -p flux-system` (+ schema crate), clippy `-D warnings`,
      fmt, `flux-codegate`.

## Progress
- (not started — needs [D-25](D-25-endpoint-reference-model.md), [D-26](D-26-endpoint-discovery-broker.md),
  and **[D-20](D-20-scoped-private-net-egress.md)** as a hard prerequisite.)

## Notes
- **Honors AGENTS.md "no bypass paths"** — resolution + IO stay behind `Executor::dispatch`; this makes
  the envelope finer-grained, it does not skip it. Mirrors D-20's framing.
- Reuse: `resolve_auth`/`resolve_purpose` injection (`crates/flux-plugin/src/lib.rs:586–616`); the
  D-20 scoped allow-set; the `flux-events` audit substrate (D-02).
- Blocks [D-28](D-28-kubernetes-endpoint-provider.md), [D-29](D-29-migrate-plugins-to-references.md).
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).
