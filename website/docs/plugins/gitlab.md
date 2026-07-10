---
title: GitLab plugin
description: "Step-by-step setup for the gitlab plugin: install, authenticate, wire a self-hosted instance, and call an operation."
---

# GitLab plugin

A worked setup for the `gitlab` plugin — projects, merge requests, issues, pipelines, releases, and
repository file/branch/tag operations against a GitLab instance. This page walks through the exact
sequence for a real self-hosted instance, using only the `flux` CLI. For the general plugin mechanics
(capability grants, trust model, everyday commands), see [Using plugins](./using-plugins.md).

## 1. Install

```bash
flux plugin install gitlab
```

This resolves the newest signed `plugins-v*` pack release, verifies the index signature and the
archive's sha256, and unpacks the binary into the versioned store. Confirm it landed:

```bash
flux plugin status gitlab
```

```text
gitlab           ~/.flux/plugins/bin/gitlab/0.1.0/flux-plugin-gitlab   v0.1.0  [ok]  [verified]
    manifest:  v0.1.0  79 op(s)  ·  1 auth purpose(s)  ·  1 endpoint(s)  ·  3 datasource(s)  ·  caps: http, secret(2), blob
    auth:      · personal_token — not configured (env: GITLAB_PERSONAL_TOKEN, GITLAB_PERSONAL_ACCESS_TOKEN)
    endpoint:  · gitlab.endpoint — env not set, defaults to https://gitlab.com
```

`ok`/`verified` only proves the binary launched and its hash matches the signed descriptor — it does
**not** mean authentication or network access has been checked. The `auth:`/`endpoint:` lines below it
are the wiring itself: which env var each declared purpose reads (never the value), and whether one is
currently set. This is what's actually true right now, not a promise about what will work at call time.

## 2. Provide the token and (for self-hosted) the base URL

`status` just told you the exact env vars this plugin's manifest declares — two per purpose, tried in
order:

| Purpose | Env vars (first one set wins) |
|---|---|
| Personal access token | `GITLAB_PERSONAL_TOKEN`, `GITLAB_PERSONAL_ACCESS_TOKEN` |
| Base URL (omit for `gitlab.com`) | `GITLAB_URL`, `GITLAB_BASE_URL` |

```bash
export GITLAB_PERSONAL_TOKEN="glpat-…"          # a token with API scope
export GITLAB_URL="https://gitlab.example.com"  # self-hosted only; unset = gitlab.com
```

Re-run `flux plugin status gitlab` and both lines flip to `✓`, with the endpoint line showing the
resolved base URL (endpoints are not secret — `flux endpoint show`/`resolve` print them too):

```text
    auth:      ✓ personal_token — env $GITLAB_PERSONAL_TOKEN
    endpoint:  ✓ gitlab.endpoint — https://gitlab.example.com (env $GITLAB_URL)
```

Note what `status` never shows: the token value itself. These env vars never need to be written into
`.flux/config.toml` or read by anything flux-side other than the host process. The plugin subprocess
never sees them as OS environment variables at all — it is spawned with a cleared environment and
requests the token by name over an IPC capability call, which the host resolves from its own env and
hands back only because the plugin's manifest declared these exact keys as a granted secret. See
[Credentials & secrets](../security/credentials.md) for the full resolution path.

## 3. Self-hosted on a private network? Grant egress

A GitLab instance on an internal/private address (RFC1918, `*.internal`, VPN-only DNS, etc.) is
refused by the SSRF guard by default — you'll see this if you skip this step:

```text
error: plugin `gitlab` op `gitlab.test`: provider error: refusing to fetch private/loopback/link-local address …
```

Grant the specific host in `.flux/config.toml` (project) or `~/.flux/config.toml` (user default):

```toml
[private_net.plugins]
gitlab = ["gitlab.example.com"]
```

The grant is intersected with what the plugin itself declares, so this only ever opens the one host
you name. Public `gitlab.com` needs no grant. See
[Private-network egress](../reference/config.md#private-network-egress) for the full mechanism.

## 4. Verify

```bash
flux plugin call gitlab gitlab.test
```

`gitlab.test` fetches the current authenticated user — the cheapest possible end-to-end check that
the token and endpoint are both wired correctly:

```json
{
  "status": "ok",
  "text": "GitLab auth OK",
  "user": { "id": 1, "username": "…", "name": "…", "web_url": "https://gitlab.example.com/…" }
}
```

An auth failure surfaces as a `401` from GitLab, not a flux-side error — that tells you the token is
wrong or lacks scope, not that the wiring is broken. A private-network refusal (step 3) means the
wiring reached the network guard but not GitLab yet.

## 5. Call a real operation

Any of the plugin's declared operations works the same way — `flux plugin call gitlab <op> [json]`,
or let an agent call them once it's running with the plugin installed:

```bash
flux plugin call gitlab gitlab.project.list '{"per_page": 5}'
flux plugin call gitlab gitlab.mr.list '{"project": "group/project", "state": "opened"}'
flux plugin call gitlab gitlab.issue.create '{"project": "group/project", "title": "…"}' --dry-run
```

`--dry-run` spawns the plugin once to read its manifest and validates the input against the
operation's schema, but it never invokes the operation or performs its operation-level network/write
work. It is useful for checking argument shape before a write operation.

## Recap

| Step | Command | Failure mode if skipped |
|---|---|---|
| Install | `flux plugin install gitlab` | `plugin \`gitlab\` not installed` |
| Token (+ URL if self-hosted) | `export GITLAB_PERSONAL_TOKEN=…` | `secret \`GITLAB_PERSONAL_TOKEN\` not set` |
| Private-net grant (self-hosted only) | `[private_net.plugins]` in config | `refusing to fetch private/loopback/link-local address …` |
| Verify | `flux plugin call gitlab gitlab.test` | (this *is* the verification step) |

## Related docs

- [Using plugins](./using-plugins.md) — install, pin, capability grants, and the trust model shared by
  every plugin.
- [Credentials & secrets](../security/credentials.md) — how the token resolves without the plugin ever
  seeing raw environment variables.
- [Configuration](../reference/config.md) — `[private_net.plugins]` and other project/user config.
- [Plugin capability sandbox](../security/plugin-sandbox.md) — the manifest fields behind these grants.
