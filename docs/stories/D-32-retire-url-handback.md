---
id: D-32
title: Retire the host.endpoint URL-handback (complete the references-only cutover)
pillar: Core
status: done
epic: endpoint-discovery
design: docs/designs/endpoint-discovery.md
note: the endpoint URL-handback is GONE (SystemHostCaps arm + Host::endpoint + MockHost deleted, compile-enforced) — covered by a gated non-secret config capability (refuses secret keys AND credential-bearing values), template endpoints composing host-side dynamic bases (Atlassian gateway), http_bytes_ref for attachment byte-IO, and sql dialing by named ref; scope grew from the story's 5–6 residuals to 13 plugin migrations
---

# Retire the `host.endpoint` URL-handback (complete the references-only cutover)

## Goal
Remove the `host.endpoint(name) → url` capability entirely (and the host-kit `Host::endpoint` client
method), so **no** plugin op ever receives a URL string — completing the clean cutover D-29 started.
This requires three small host capabilities to cover the residual uses that still need the handback.

## Why
D-29 migrated every plugin's **primary** IO to reference-based calls, but `host.endpoint` is retained
for three narrow residuals (documented in D-29): (1) attachment **byte-IO** (`http_bytes` has no ref
variant), (2) jira's **constructed Atlassian gateway** URL (`api.atlassian.com/ex/jira/{cloud_id}` — a
dynamic URL, not a static named endpoint) and its `cloud_id`/`email` **config-value** reads, and (3)
`sql`'s static env endpoint path. Until those are covered, the references-only invariant holds for the
primary IO surface but not 100%. This story closes the remaining 5–6 call sites and deletes the
capability so the invariant is compile-enforced. See [endpoint-discovery.md](../designs/endpoint-discovery.md).

## Acceptance
- [x] **`http_bytes_ref`** — a binary-body/binary-response variant of the ref-based HTTP capability +
      host-kit helper; migrate confluence/jira attachment byte-IO to it.
- [x] **Dynamic-endpoint resolution** — a way for a plugin to reach a host-composed dynamic base (the
      Atlassian gateway from `cloud_id`) by reference, without holding the URL (e.g. a parameterized
      named endpoint or a `gateway`-style resolver input). Migrate jira/confluence's gateway path.
- [x] **Non-secret `config` read** — a gated capability for a plugin to read a declared **non-secret**
      config value (jira `cloud_id`/`email`) without abusing `host.endpoint`.
- [x] **`sql` static path** — migrate `sql`'s static env endpoint to the named-ref path (the
      `SystemHostCaps` local resolution already supports it).
- [x] **Cutover** — delete the `endpoint` host capability handler in `SystemHostCaps` and `Host::endpoint`
      in host-kit; the **workspace + plugins build is the proof** (any remaining caller fails to compile).
- [x] Gate green across both workspaces; `flux-codegate`; clippy `-D warnings`; fmt.

## Progress
- **Done (2026-07-02).** All four covering capabilities landed, then the handback deleted:
  - **`config` host capability** — declared via `PluginManifest.config: Vec<ConfigSpec {name, env,
    description}>`, deny-by-default (undeclared names refused). Hardened beyond the story: refuses
    secret-classified env keys (granted `secrets` or auth-method envs) AND refuses
    credential-bearing URL values (a DSN with an embedded password errors "embeds a credential").
    Guest side: `Host::config(name)`, `PluginBuilder::config`, `MockHost::with_config`.
  - **Template endpoints** — `EndpointSpec.template` composes a host-side dynamic base from
    declared config values (`{name}` placeholders, percent-encoded); the composed host feeds the
    HTTP allow-list. jira/confluence gateways became template endpoints
    (`https://api.atlassian.com/ex/{jira|confluence}/{cloud_id}`) — the plugin never holds the URL,
    and their old pseudo-endpoint/fake-auth-method probes for cloud_id/email are gone.
  - **`http_bytes_ref`** — guest helper for binary byte-IO by reference (host's `http.do` already
    handled ref + `body_b64` + `response_binary`); jira/confluence attachments migrated, gitlab's
    archive download is now byte-exact. `http_ref` gained a `headers` param (runtime session
    tokens: homer JWT, gitlab PRIVATE-TOKEN, opsgenie GenieKey, loki X-Scope-OrgID);
    `send_json_ref` now sets `content-type: application/json` on the ref path.
  - **sql** — dials by `conn_dial_ref("sql.endpoint")`; `SqlTarget.host`/`port` deleted (the plugin
    cannot dial a parsed address); DSN metadata via `config("dsn")`. The bare discovered
    `@endpoint/<id>` string input was removed — it depended on the handback and only ever worked
    against mocks; a clear error now directs to the full `endpoint` object from `endpoint.select`.
  - **Deletions** — `SystemHostCaps`' `"endpoint"` arm, host-kit `Host::endpoint`,
    `MockHost::{endpoints, with_endpoint}`; a flux-plugin test now asserts `endpoint` is an
    unknown host capability.
  - **Scope extension** — the story counted 5–6 residuals; the tree had grown to 13 consumer
    plugins (homer ×8 call sites, gitlab, loki, prometheus, opsgenie, asterisk, slack, HF, …) —
    all migrated. `EndpointSpec.default` (host-side default base) replaced plugin-side URL
    fallbacks behavior-preservingly; asterisk's host:port moved to `config` with defaults kept.
  - **Tests (failing-first):** flux-plugin `config_capability_reads_declared_non_secret_values_only`,
    `config_capability_refuses_credential_bearing_urls`, `template_endpoint_composes_from_config`,
    `endpoint_default_url_resolves_when_env_unset`, `dial_target_from_url_defaults_sql_scheme_ports`;
    host-kit `http_bytes_ref_round_trips_binary_by_reference`, `config_reads_declared_value_and_errors_on_unknown`;
    jira/confluence gateway-by-ref + attachment-byte-IO tests; sql `static_endpoint_dials_by_reference`.
  - **Gate:** both workspaces green (root `cargo test --workspace` + clippy + fmt + codegate;
    plugins `cargo build --workspace --all-targets` — the compile-enforced cutover proof — +
    `cargo test --workspace` + clippy + fmt), re-verified by the orchestrator after merge.
- Residual: `plugins/docker/src/main.rs:35` mentions `host.endpoint(...)` in a comment describing
  fluxplane's architecture (historical comparison) — intentionally left; no code remnant exists.

## Notes
- The cutover is compile-enforced: removing `Host::endpoint` turns any straggler into a build error.
- Keep the manifest `endpoint`/`auth` declarations (host-side resolver defaults) — only the
  URL-handback *capability* is removed.
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).
