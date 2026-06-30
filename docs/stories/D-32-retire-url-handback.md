---
id: D-32
title: Retire the host.endpoint URL-handback (complete the references-only cutover)
pillar: Core
status: backlog
epic: endpoint-discovery
design: docs/designs/endpoint-discovery.md
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
- [ ] **`http_bytes_ref`** — a binary-body/binary-response variant of the ref-based HTTP capability +
      host-kit helper; migrate confluence/jira attachment byte-IO to it.
- [ ] **Dynamic-endpoint resolution** — a way for a plugin to reach a host-composed dynamic base (the
      Atlassian gateway from `cloud_id`) by reference, without holding the URL (e.g. a parameterized
      named endpoint or a `gateway`-style resolver input). Migrate jira/confluence's gateway path.
- [ ] **Non-secret `config` read** — a gated capability for a plugin to read a declared **non-secret**
      config value (jira `cloud_id`/`email`) without abusing `host.endpoint`.
- [ ] **`sql` static path** — migrate `sql`'s static env endpoint to the named-ref path (the
      `SystemHostCaps` local resolution already supports it).
- [ ] **Cutover** — delete the `endpoint` host capability handler in `SystemHostCaps` and `Host::endpoint`
      in host-kit; the **workspace + plugins build is the proof** (any remaining caller fails to compile).
- [ ] Gate green across both workspaces; `flux-codegate`; clippy `-D warnings`; fmt.

## Progress
- (not started — follow-up to D-29; small but spans host-kit + flux-plugin + confluence/jira/sql.)

## Notes
- The cutover is compile-enforced: removing `Host::endpoint` turns any straggler into a build error.
- Keep the manifest `endpoint`/`auth` declarations (host-side resolver defaults) — only the
  URL-handback *capability* is removed.
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).
