---
title: Plugin capability sandbox
description: How flux confines plugin host callbacks — deny-by-default capabilities, credential paths, and the manifest fields that declare them.
---

# Plugin capability sandbox

The plugin sandbox is flux's confinement model for plugin capabilities. It governs what a plugin can
ask the host to do — HTTP, subprocesses, secrets, connections, and private-network access — while the
plugin is running.

For the separate question of which plugin code is allowed to run at all, see
[Plugin trust & signing](./plugin-trust.md). Both rely on the same
[safety envelope](../agent/safety.md).

## Host capabilities, not an OS sandbox by default

First-party plugins are written so every privileged effect — an HTTP request, subprocess, secret
read, connection, or extra-workspace file read — is a **capability callback** the host executes on
their behalf. The process is launched with a cleared, minimal environment, so provider and host
secrets are not inherited. Undeclared host capabilities, hosts, secrets, and programs are denied by
default.

This confines what plugin code can reach *through flux*. Plugin binaries are trusted native code and
are not OS-sandboxed by default; a malicious binary could bypass the callback protocol with direct
syscalls. Within the plugin contract, the host is the single IO boundary: the plugin asks; the host
decides and performs. The [OS process sandbox](./os-sandbox.md) (`[sandbox]`) reduces that bypass
risk as defense-in-depth **underneath** this capability model: it constrains writes and can disable
the process's network whether or not the process honors the callback protocol. It does not turn
native code into a capability-safe process — v1 still exposes filesystem reads and, while the
sandbox is active, networking stays open unless configuration or the CLI's unattended profile closes
it. The CLI applies that fail-closed profile only to its documented auto-approved and `--serve`
forms; SDK/server embedders must inject or export the posture they require.

## Capabilities are deny-by-default

A plugin's manifest declares exactly which host capabilities it needs. The host grants **only** what
is declared and checks every callback against it. An empty list or a `false` flag means that
capability is denied.

| Capability | Grants | Scope |
|---|---|---|
| `process` | Run a subprocess | argv-**prefix** allow-list (empty = denied) |
| `secrets` | Materialize a declared secret or auth purpose into trusted plugin code | purpose must be a declared auth method; direct keys must be in the exact env-key allow-list |
| `http` + `http_hosts` | Make HTTP requests | boolean on/off, plus an allowed-host list (SSRF guard still applies) |
| `private_hosts` | Reach private/loopback addresses | declared hosts, admitted only when the operator *also* grants them |
| `conn` | Open a raw connection | `tcp:host:port` / `unix:/path` targets (`*` wildcards one segment) |
| `blob` | Content-addressed scratch store | boolean |
| `discover` | Ask "what endpoints exist for product X?" | boolean (cross-plugin discovery) |
| `credential` | Materialize a credential reference into trusted plugin code | boolean (see the credential paths below) |
| `fs` | Read specific host files outside the workspace | path-scoped globs, `..` rejected |

An argv-prefix grant matches whole leading tokens, not a substring: `kubectl` admits any `kubectl …`,
while `kubectl get` admits `kubectl get pods` and refuses `kubectl delete pod x`. That is what lets a
plugin be granted read verbs without the destructive ones. The manifest-level gate and the
per-operation narrowing use the same matcher, so the two levels cannot disagree.

Because these are declared once in the manifest and checked on every callback, there is no per-call
negotiation a compromised plugin could talk its way through. The manifest is the plugin-side upper
bound; operator grants may narrow private-network and cross-plugin credential access further.

## Credential paths

The safest HTTP path is reference-only:

- With an **endpoint reference**, the host resolves the base URL (from declared environment, a
  default, or a host-composed template), runs it through the SSRF guard and host allow-list, and
  makes the request. The composed URL never crosses back to the plugin on this path.
- It requests auth by **purpose** (`"api_token"`), never by value. For a host-mediated `http.do`
  request, the host resolves the secret and injects it as the declared scheme (Bearer, Basic, a named
  header, or a query parameter). The token is not serialized back to the plugin on this path.

The plugin drives that request without holding a URL-with-credentials or a token. Not every protocol
can use that path, so the manifest also exposes explicit, deny-by-default ways for trusted plugin code
to receive material:

1. **`secret`** resolves a declared auth purpose or allow-listed environment key and returns the raw
   value to the trusted plugin. This is how integrations that must construct their own protocol
   request obtain a token. A stored token, OAuth bearer, or environment value can take this path.
2. **`credential`** resolves an endpoint or credential reference and returns the raw value to the
   trusted plugin. It is refused unless the manifest sets `credential = true`; discovery and endpoint
   lookup never materialize the value implicitly.
3. **`conn.authenticate`** avoids materialization into plugin code. The plugin opens a granted socket,
   then the host resolves the password and speaks the supported Postgres or MySQL handshake. The
   plugin receives the post-authentication connection and negotiated non-secret parameters, not the
   password.

Values materialized through `secret` or `credential`, and values used by `conn.authenticate`, are
registered with the [redactor](./credentials.md). They may reach trusted plugin code where stated,
but never model-visible output by design.

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

The host-callback surface a plugin receives is what its manifest declares. The two blocks that matter
most for auth:

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
      "endpoint": "gitlab.endpoint",
      "authorize_path": "/oauth/authorize",
      "token_path": "/oauth/token",
      "client_id": "flux-cli",
      "scopes": ["read_api"],
      "grants": ["authorization_code", "refresh_token"],
      "redirect": { "port": 1456, "path": "/auth/callback" }
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
- [OS process sandboxing](./os-sandbox.md) — interactive defaults, the CLI's fail-closed forms, and
  embedder responsibilities for the raw plugin process.
