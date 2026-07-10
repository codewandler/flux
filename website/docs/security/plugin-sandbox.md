---
title: Plugin capability sandbox
description: How flux confines what a plugin's code can reach — deny-by-default capabilities, references-only IO, and the manifest fields that declare them.
---

# Plugin capability sandbox

The plugin sandbox is flux's confinement model for plugin capabilities. It governs what a plugin can
ask the host to do — HTTP, subprocesses, secrets, connections, and private-network access — while the
plugin is running.

For the separate question of which plugin code is allowed to run at all, see
[Plugin trust & signing](./plugin-trust.md). Both rely on the same
[safety envelope](../agent/safety.md).

## Host capabilities, not an OS sandbox

First-party plugins are written so every privileged effect — an HTTP request, subprocess, secret
read, connection, or extra-workspace file read — is a **capability callback** the host executes on
their behalf. The process is launched with a cleared, minimal environment, so provider and host
secrets are not inherited. Undeclared host capabilities, hosts, secrets, and programs are denied by
default.

This confines what plugin code can reach *through flux*. Plugin binaries are trusted native code and
are not OS-sandboxed; a malicious binary could bypass the callback protocol with direct syscalls.
Within the plugin contract, the host is the single IO boundary: the plugin asks; the host decides
and performs.

## Capabilities are deny-by-default

A plugin's manifest declares exactly which host capabilities it needs. The host grants **only** what
is declared and checks every callback against it. An empty list or a `false` flag means that
capability is denied.

| Capability | Grants | Scope |
|---|---|---|
| `process` | Run a subprocess | exact `argv[0]` allow-list (empty = denied) |
| `secrets` | Read a secret by env key | exact env-key allow-list (empty = denied) |
| `http` + `http_hosts` | Make HTTP requests | boolean on/off, plus an allowed-host list (SSRF guard still applies) |
| `private_hosts` | Reach private/loopback addresses | declared hosts, admitted only when the operator *also* grants them |
| `conn` | Open a raw connection | `tcp:host:port` / `unix:/path` targets (`*` wildcards one segment) |
| `blob` | Content-addressed scratch store | boolean |
| `discover` | Ask "what endpoints exist for product X?" | boolean (cross-plugin discovery) |
| `credential` | Materialize a credential reference into its raw value | boolean (see the exceptions below) |
| `fs` | Read specific host files outside the workspace | path-scoped globs, `..` rejected |

Because these are declared once in the manifest and authorized up front, there is no per-call
negotiation a compromised plugin could talk its way through — the manifest is the single source of
truth.

## References-only IO

The confinement above would be hollow if a plugin still received the credential it wanted to use. It
doesn't. On the normal IO paths, a plugin deals only in **references**:

- It addresses an API by an **endpoint reference**, never a URL. The host resolves the reference to a
  base URL (from declared environment, a default, or a host-composed template), runs it through the
  SSRF guard and host allow-list, and makes the request. The composed URL never crosses back to the
  plugin.
- It requests auth by **purpose** (`"api_token"`), never by value. The host resolves the secret and
  injects it as the declared scheme (Bearer / Basic / a named header / a query parameter). The token
  is applied host-side and is never serialized back to the plugin.

So the plugin drives the request but never holds the URL-with-credentials or the token itself.

## The two audited exceptions

Two protocols need the raw secret *on the wire* — for example a Postgres client doing in-band SCRAM
authentication. flux handles these with two narrowly-scoped, deny-by-default paths. In both, the raw
value reaches the **trusted plugin binary only, never the model**, and is registered with the
[redactor](./credentials.md) so it can't leak into output:

1. **The `credential` capability** materializes a credential reference into its value and hands it to
   the plugin binary. It is refused unless the manifest declared it, and the value is never returned
   through any discovery or endpoint path — only through this explicit, audited capability.
2. **`conn.authenticate`** is the better pattern, and it closes even that gap: the plugin dials the
   socket, but the **host** speaks the Postgres startup and SCRAM-SHA-256 handshake (verifying the
   server signature, with a bounded iteration count) and hands the plugin a *post-authentication*
   connection. Even the raw-socket plugin never receives the password — it gets only the negotiated,
   non-secret connection parameters.

## Cross-plugin and private-network grants

Two boundaries need the operator's explicit say-so, not just a manifest declaration:

- **Cross-plugin credentials.** When one plugin uses a credential owned by another, resolution is
  gated by a three-part check: an operator grant for that consumer, a first-use approval, and an
  audit record.
- **Private-network egress.** Reaching a private/loopback host requires that the plugin *declared* it
  **and** the operator *granted* it. The host intersects the two — you can't grant a host the manifest
  never named — and every admitted private-address call is audited. This is the model behind the
  `[private_net.plugins]` grants in [Configuration](../reference/config.md).

## Manifest reference: declaring the surface

The security surface a plugin exposes is exactly what its manifest declares. The two blocks that
matter most for auth:

**Capabilities** — the deny-by-default grant set from the table above. For example:

```json
"capabilities": {
  "http": true,
  "http_hosts": ["gitlab.com"],
  "secrets": ["GITLAB_TOKEN"],
  "private_hosts": ["gitlab.internal.example"]
}
```

**An `oauth2`-backed auth method** — declares that a purpose is OAuth2-backed so the host runs the
grants (see [Credentials & secrets](./credentials.md) for the login flow):

```json
"auth": [
  { "purpose": "api_token", "scheme": "bearer", "env": ["GITLAB_TOKEN"],
    "oauth2": {
      "endpoint": "gitlab.endpoint",      // a DECLARED endpoint name — the host builds the token URL
      "authorize_path": "/oauth/authorize",
      "token_path": "/oauth/token",
      "client_id": "flux-cli",
      "scopes": ["read_api"],
      "grants": ["authorization_code", "refresh_token"],
      "redirect": { "port": 1456, "path": "/auth/callback" }  // loopback 127.0.0.1 listener
    } }
]
```

The key detail: `endpoint` names a **declared** endpoint. The host joins `authorize_path` /
`token_path` onto that base URL, so the token host stays host-controlled and egress-gated — a plugin
can never point the OAuth flow at a URL of its choosing. The `redirect` is a local `127.0.0.1`
listener, so it's outside the outbound allow-list by construction.

For authoring a plugin end to end, see [Plugin authoring](../plugins/authoring.md).

## Related docs

- [Plugin trust & signing](./plugin-trust.md) — identity and integrity of installed binaries.
- [Using plugins](../plugins/using-plugins.md) — install, pin, and grant access.
- [Plugin authoring](../plugins/authoring.md) — write manifests that declare capabilities honestly.
