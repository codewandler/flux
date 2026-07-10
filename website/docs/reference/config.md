---
title: Configuration
description: "Complete .flux/config.toml reference: precedence, permissions, network grants, limits, skills, workspace, endpoints, and server settings."
---

# Configuration

flux works without a config file. User defaults live in `~/.flux/config.toml`; a project can add
`.flux/config.toml` at its workspace root.

The broad precedence is CLI flags > project config > user config > built-in defaults, but merging is
intentional rather than simple replacement:

- scalar values use the project value when present;
- permission lists, policy grants, endpoint credential grants, and private-network host lists merge;
- custom skill directories use project-before-user order because the first skill name wins;
- `enable_shell` and `workspace.allow_all` are enabled if either layer enables them.

An “always allow” choice at an approval prompt is persisted to the project config.

## Representative configuration

```toml
model = "sonnet"
browser_bin = "/usr/bin/chromium"
enable_shell = false

[permissions]
allow = ["read", "glob", "grep", "search", "Bash(git:*)"]
deny  = ["Bash(rm:*)"]

[private_net]
web = ["docs.internal.example"]

[private_net.plugins]
prometheus = ["prometheus.internal.example"]

[private_net.endpoints]
"gitlab:gitlab.endpoint" = ["gitlab.internal.example"]

[limits]
turn_token_budget = 120000
readonly_rounds_escalate = 6
readonly_rounds_stop = 10

[skills]
dirs = ["team/skills"]

[workspace]
add_dirs = ["../shared-docs"]
allow_all = false

[endpoint]
cross_plugin_credentials = ["sql:kubernetes"]

[[endpoint.static]]
id = "pg-prod"
url = "postgres://db.example:5432/app"
product = "postgres"
protocol = "postgres"
credential_ref = "env/POSTGRES_PASSWORD"
labels = { environment = "production" }

[server]
a2a_session_ttl_secs = 3600
external_url = "https://agents.example.com"

[[policy.grants]]
subjects  = [{ kind = "user", id = "*" }]
resources = [{ kind = "path", path = "src/**" }]
actions   = ["workspace.write"]
```

## Top-level settings

| Key | Meaning |
|---|---|
| `model` | Default provider/model spec; `-m` overrides it. Default: `sonnet`. |
| `browser_bin` | Chromium executable for `browser.*`; otherwise `FLUX_BROWSER_BIN`, then `PATH`. |
| `enable_shell` | Surface the high-risk `bash` and `proc.run` shell group. Off by default. |
| `allow_private_net` | Deprecated compatibility switch that grants all private hosts to native web ops. Prefer `[private_net] web`. It never grants plugins. |

## Permissions and policy

`[permissions]` is the ergonomic approval layer: deny rules are evaluated first, then allow rules;
otherwise the operation prompts. Entries may be operation names (`read`, `search`) or scoped shell
subjects such as `Bash(git:*)`. Reads are pre-allowed by the local defaults.

`[[policy.grants]]` adds fine-grained authorization grants to the built-in policy floor. Permission
rules cannot widen past that floor, and destructive operations always re-fire the approval gate.
Interactive runs prompt; `--yes` answers every gate “yes,” including destructive ones. See
[Safety & approvals](../agent/safety.md).

## Private-network egress

DNS is resolved before a request and private, loopback, link-local, unique-local, and internal hosts
are refused by default.

- `[private_net] web` covers the entire native web family: `http.request`, `web_fetch`, and
  `browser.*`. Use a host list, or `true` for any private address.
- `[private_net.plugins]` is keyed by plugin manifest name. The host intersects the operator grant
  with hosts declared by that plugin.
- `[private_net.endpoints]` is keyed by `"<plugin>:<endpoint-name>"` and merges with the owning
  plugin's grant.

The former `[private_net] web_fetch = …` key is no longer read; migrate it to `web`.

For a one-off invocation, global `--allow-private-net` temporarily opens native web to every private
range and supplies the operator side of plugin grants:

```bash
flux --allow-private-net plugin call gitlab gitlab.test
```

The plugin still cannot use a host absent from its manifest. Native web has no manifest intersection,
so the flag also admits cloud-metadata addresses for that run; prefer scoped config for recurring
access. Every admitted private request is audited.

## Resource limits

| `[limits]` key | Default | Meaning |
|---|---:|---|
| `turn_token_budget` | off | Stop consulting models after cumulative turn usage crosses this ceiling. `--turn-budget`, then `FLUX_TURN_TOKEN_BUDGET`, override it. |
| `readonly_rounds_escalate` | `6` | Consecutive read-only planner rounds before an “answer now” escalation; `0` disables. |
| `readonly_rounds_stop` | `10` | Consecutive read-only planner rounds before the turn stops honestly; `0` disables. |

## Skills and workspace access

`[skills] dirs` adds skill directories above the well-known project/global set. Relative paths are
resolved from the workspace; `~/` expands to the home directory. Earlier directories win a name
collision. CLI `--skill-dir` entries have the highest precedence. See [Skills & roles](../agent/skills-and-roles.md).

`[workspace] add_dirs` grants extra **read-only** roots outside the workspace; writes remain confined
to the workspace. It mirrors repeatable `--add-dir`. `allow_all = true` mirrors
`--allow-all-paths`, removes read and write confinement, and prints a warning—use it only when full
host access is intentional.

## OS-level process sandbox

`[sandbox]` turns on opt-in OS-level confinement (bubblewrap on Linux, Seatbelt on macOS) for every
process flux spawns — shell/exec ops and plugin subprocesses alike — as defense-in-depth
underneath the safety envelope. Off by default:

```toml
[sandbox]
enabled = true      # turn on OS sandboxing for spawned processes
require = false     # fail closed instead of warn-and-continue when no backend is usable (implies enabled)
network = true       # omit for the unrestricted default; false closes the sandbox's network namespace/profile
writable = ["../shared-output"]   # extra writable paths beyond the workspace root and toolchain caches
```

| `[sandbox]` key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Turn on OS sandboxing for spawned processes. |
| `require` | `false` | Fail closed (refuse to spawn) instead of warning when no sandbox backend is usable. Implies `enabled`. |
| `network` | unset (unrestricted) | Whether sandboxed processes may reach the network. `false` closes the sandbox's network namespace/profile. |
| `writable` | `[]` | Extra writable paths, beyond the workspace root, named/Git-worktree roots, `/tmp`/`$TMPDIR`, and the toolchain caches. A leading `~/` expands to the home directory. Missing configured paths are created as directories before launch; `/` is rejected (use the explicit `--allow-all-paths` hatch instead). |

Merge is security-directional, not the ordinary "project wins" rule: `enabled`/`require` are OR'd
(a project may tighten a user's posture, never loosen it), `network` is strictest-wins (`false`
beats `true`/unset), and `writable` concatenates — the same documented widening as
`[workspace] add_dirs`.

The global `--sandbox`/`--no-sandbox` flags and the `FLUX_SANDBOX`/`FLUX_SANDBOX_NET`/
`FLUX_SANDBOX_WRITABLE` environment variables resolve **tightest-wins**: the strictest posture any
source asks for takes effect (`require` beats `on` beats off), so `--sandbox` layered over
`[sandbox] require = true` stays `require` rather than weakening it. The one exception is the
explicit kill switch — `--no-sandbox` or `FLUX_SANDBOX=off` — which forces sandboxing off outright.
An unrecognized or empty `FLUX_SANDBOX` value is ignored (it never downgrades a configured posture),
and a config file that fails to parse is a hard startup error rather than silently dropping a
configured `require`.
The CLI exports the resolved posture so a child flux invocation (`app run`, an eval sub-agent,
`plugin call`) inherits it. See [OS process sandboxing](../security/os-sandbox.md) for the full
reference — platform coverage, the posture matrix, the browser exemption, and what v1 does not
defend against.

## Endpoint brokerage

`[endpoint] cross_plugin_credentials` grants a consumer plugin permission to use a credential owned
by another provider plugin. Entries are `"<consumer>:<provider>"`; `"consumer:*"` grants that
consumer any provider. The default is deny. This is only one part of the gate: first use still needs
approval and is audited.

Each `[[endpoint.static]]` table declares a named, weak endpoint reference. `id` and a
credential-free `url` are required; `product`, `protocol`, `credential_ref`, and non-secret
`labels` are optional. A credential reference is a location such as `env/PGPASSWORD`,
`kubernetes/<namespace>/<secret>/<key>`, or `plugin/<plugin>/<instance>/<slot>`—never a secret
value. Project declarations override user declarations with the same id. Use `flux endpoint add`
when you want the equivalent imperative surface. See [Endpoints](../agent/endpoints.md).

## Server settings

| `[server]` key | Meaning |
|---|---|
| `a2a_session_ttl_secs` | Idle lifetime for A2A-created sessions; default `3600`, `0` disables pruning. |
| `external_url` | Trusted public origin advertised in agent cards; required with token introspection. |
| `introspect_url` | RFC 7662 bearer-token introspection endpoint; enables per-principal isolation. |
| `introspect_client_id` | Optional client id for `client_secret_basic`. |
| `introspect_client_secret_env` | Environment-variable **name** holding the client secret. |
| `introspect_account_claim` | Claim or dot-path carrying the account/tenant id. |
| `introspect_roles_claim` | Claim carrying roles as an array or space-separated string. |
| `introspect_require_account` | Reject tokens with no account claim. |
| `introspect_allow_http` | Permit a plaintext introspection endpoint; off by default. |

See [Server authentication & tenancy](../security/server-auth.md) before exposing a non-loopback
listener.

## Environment overrides

Common runtime overrides include `FLUX_VERBOSE=1`, `FLUX_SHOW_LOOP=1`,
`FLUX_TURN_TOKEN_BUDGET`, `FLUX_COMPACT_CHARS`, `FLUX_ENABLE_BASH=1`,
`FLUX_BROWSER_BIN`, `FLUX_ALLOW_PRIVATE_NET=1`, `OLLAMA_HOST`, `FLUX_SANDBOX` (`off`/`on`/`require`),
`FLUX_SANDBOX_NET`, `FLUX_SANDBOX_WRITABLE`, `FLUX_BWRAP_BIN`, `FLUX_SANDBOX_EXEC_BIN`, and the
provider API-key variables listed under [Providers and models](../agent/providers.md).
Security-relevant booleans only enable on `1`, `true`, `yes`, or `on`; values such as `0` and
`false` stay off.

## Related docs

- [Safety & approvals](../agent/safety.md) — policy, permissions, and destructive re-checks.
- [Skills & roles](../agent/skills-and-roles.md) — discovery and precedence.
- [Endpoints](../agent/endpoints.md) — weak references and cross-plugin credentials.
- [Credentials & secrets](../security/credentials.md) — token storage and redaction.
- [OS process sandboxing](../security/os-sandbox.md) — the full `[sandbox]` reference.
