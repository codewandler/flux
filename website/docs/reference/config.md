---
title: Configuration
description: "Complete .flux/config.toml reference: precedence, permissions, network grants, limits, skills, workspace, endpoints, and server settings."
---

# Configuration

flux works without a config file. User defaults live in `~/.flux/config.toml`; a project can add
`.flux/config.toml` at its workspace root. An operator can additionally pin an organization-wide
floor ahead of both — see [Managed configuration tier](#managed-configuration-tier-operator-floor)
below.

The broad precedence is CLI flags > project config > user config > managed config > built-in
defaults, but merging is intentional rather than simple replacement:

- scalar values use the project value when present;
- permission lists, policy grants, endpoint credential grants, private-network host lists, and the
  `[tools] disable` list merge;
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

[agent]
loop = "adaptive"

[private_net]
web = ["docs.internal.example"]

[private_net.plugins]
prometheus = ["prometheus.internal.example"]

[private_net.endpoints]
"gitlab:gitlab.endpoint" = ["gitlab.internal.example"]

[limits]
turn_token_budget = 120000

[skills]
dirs = ["team/skills"]
model_invoked = false

[workspace]
add_dirs = ["../shared-docs"]
allow_all = false

[tools]
disable = ["browser.*", "web.*"]

[consult]
model = "openrouter/anthropic/claude-opus-4.6"
max_calls = 2

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
| `theme` | TUI color theme — `dark`, `light`, `dracula`, `nord`, `high-contrast`, `mono`. The in-TUI `/theme` command persists it here. See [the TUI](../agent/tui.md). |
| `browser_bin` | Chromium executable for `browser.*`; otherwise `FLUX_BROWSER_BIN`, then `PATH`. |
| `enable_shell` | Surface the high-risk `bash` and `proc.run` shell group. Off by default. |
| `allow_private_net` | Deprecated compatibility switch that grants all private hosts to native web ops. Prefer `[private_net] web`. It never grants plugins. |

## Agent loop and typed model stages

`[agent] loop` is `"adaptive"` (the default) or a workspace-relative Flux-Lang file. Selection is
explicit: `.flux/agent-loop.flux` has no effect merely because it exists.

The shipped adaptive loop defaults to at most 50 provider calls across intent repair, exploration,
and durable decision resumes. The separate authored decision/batch repeat also defaults to 50
iterations and can be configured alongside the loop selector:

```toml
[agent]
loop = "adaptive"
max_iterations = 50
```

Its two built-in stages inherit the agent model, effort, and token limit unless overridden:

```toml
[agent.adaptive]
max_model_calls = 50

[agent.adaptive.intent]
model = "codex/gpt-5.5" # optional; must use the agent's provider
effort = "low"
max_tokens = 1024
max_calls = 2

[agent.adaptive.explore]
effort = "high"
max_tokens = 8192
max_calls = 8
```

All ceilings must be greater than zero. A matching provider prefix is accepted and stripped; a
different provider fails during startup rather than opening another credential path. The CLI
`--max-model-calls` overrides the configured provider-call total for one invocation;
`--max-iterations` independently overrides the outer repeat and accepts 1 through 1,000. An
authored `ai_segment.max_rounds` is its own exact local provider-call ceiling and is not clamped to
either default.

Config may register model-backed stages as ordinary typed guarded operations. Input and output have
independent JSON Schemas; there is no common stage envelope:

```toml
[agent]
loop = "loops/support.flux"

[agent.stages.classify]
prompt = "Classify the support request and return its typed result."
input_schema = { type = "object", properties = { text = { type = "string" } }, required = ["text"], additionalProperties = false }
output_schema = { type = "object", properties = { queue = { type = "string" }, urgent = { type = "boolean" } }, required = ["queue", "urgent"], additionalProperties = false }
tools = ["search"]
model = "google/gemini-2.5-flash"
max_tokens = 768
effort = "low"
```

`tools` is a hard gather-only ceiling. Each named operation must be registered, visible to the
agent, low-risk, side-effect-free, and non-mutating. A fresh, non-cacheable read is valid; its
idempotency controls reuse rather than safety. The stage cannot use the list to gain authority or
execute writes while reasoning.

## Permissions and policy

`[permissions]` is the ergonomic approval layer: deny rules are evaluated first, then allow rules;
otherwise the operation prompts. Entries may be operation names (`read`, `search`) or scoped shell
subjects such as `Bash(git:*)`. Reads are pre-allowed by the local defaults.

For `flux app run`, these host rules are evaluated **inside** any `permissions` ceiling declared by
the `.flux` program and its owning agent. A local deny still wins; a local allow can approve a scoped
invocation but cannot restore an operation the app source removed.

`[[policy.grants]]` adds fine-grained authorization grants to the built-in policy floor. Permission
rules cannot widen past that floor, and destructive operations always re-fire the approval gate.
Interactive runs prompt; `--yes` answers every gate “yes,” including destructive ones. See
[Safety & approvals](../agent/safety.md).

## Private-network egress

DNS is resolved before a request and private, loopback, link-local, unique-local, and internal hosts
are refused by default.

- `[private_net] web` covers the entire native web family: `http.request`, `web.fetch`, and
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

## Skills and workspace access

`[skills] dirs` adds skill directories above the well-known project/global set. Relative paths are
resolved from the workspace; `~/` expands to the home directory. Earlier directories win a name
collision. CLI `--skill-dir` entries have the highest precedence. Directories only affect discovery;
skills remain inactive until selected with `--skill <name>` or an explicit `AgentSpec`.

`[skills] model_invoked` (default `false`) opts into Claude-style progressive skill disclosure:
every discovered skill's name+description is surfaced to the model, which can pull a body into
context on demand via `skill.load` — see
[Model-invoked skills (opt-in)](../agent/skills-and-roles.md#model-invoked-skills-opt-in). Mirrors
`--skills-model-invoked`; either the project or user config setting it turns it on. See
[Skills & roles](../agent/skills-and-roles.md).

`[workspace] add_dirs` grants extra **read-only** roots outside the workspace; writes remain confined
to the workspace. It mirrors repeatable `--add-dir`. `allow_all = true` mirrors
`--allow-all-paths`, removes read and write confinement, and prints a warning—use it only when full
host access is intentional.

## Tool surface (`[tools] disable`)

`[tools] disable` is a plain blocklist for turning ops off entirely — the subtractive counterpart to
evidence-gated tool groups (which only ever *add* surface as a workspace signal fires). Use it to say
"this repo never uses these ops," trimming prompt size and the operations a model could be tricked
into trying:

```toml
[tools]
disable = ["browser.*", "web.*", "bash"]
```

Each entry is either an exact op name (`"bash"`) or a `family.*` glob matching every op under that
dotted prefix (`"browser.*"` matches `browser.navigate`, `browser.click`, …; a bare `"browser"` with
no `.*` is an exact-name match only). An entry matching no known op prints a startup warning naming
it, rather than silently doing nothing — a likely typo or a stale entry naming a retired op. `/tools`
in the REPL lists every registered op and marks the disabled ones, so a mysteriously-missing op is
one command from an explanation.

**This is surface-only and defense-in-depth, not a security boundary.** A disabled op is refused if
dispatched directly too — so a cached plan or a resumed session can't call it either — but the
*authorization policy* (`[[policy.grants]]`, permission rules, approval) remains the actual security
control. If the two ever disagree, the policy wins: `[tools] disable` narrows what is offered and
dispatchable, never what an already-granted call may do.

## Second opinion (`[consult]`)

`[consult] model` names the default target the `consult` op — a second-opinion adviser that asks a
DIFFERENT model for advice on a hard sub-question, never an action — falls back to when a call
doesn't name its own `provider/model` override. Its mere presence is what surfaces `consult` into
the model-facing catalog at all: an unconfigured workspace never advertises it, so the prompt
prefix can't churn on this setting mid-session.

```toml
[consult]
model = "openrouter/anthropic/claude-opus-4.6"
max_calls = 2
```

- `model` — the default `provider/model` spec, resolved through the same routing `-m`/`--model`
  uses (subscription providers included). Absent means the op isn't registered at all.
- `max_calls` — per-turn call cap (default `2`) — a cheap second opinion, not a council of models.
  `0` refuses every call without un-surfacing the op.

See [ops reference](../language/ops.md#second-opinion) for the op's full contract (purity,
containment, usage attribution).

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

## Managed configuration tier (operator floor)

A third config layer loads **ahead of** both user and project config: a system-owned **managed**
file, read from `/etc/flux/config.toml` on Linux/macOS, or from the exact path named by
`FLUX_MANAGED_CONFIG` (the explicit channel for containerized deploys — there's no conventional
`/etc` inside a container image, and Windows deployments should use this too since there is no
wired platform convention there yet). A missing managed file is the ordinary case and changes
nothing.

The managed file is an ordinary `.flux/config.toml` document (same schema, same keys) plus one
extra table:

```toml
# /etc/flux/config.toml
[managed]
pins = ["private_net.web", "policy", "tools.disable"]

[private_net]
web = ["reports.internal.example"]   # the only host a project/user may reach; not "true"

[tools]
disable = ["browser.*"]

[[policy.grants]]
subjects  = [{ kind = "user", id = "*" }]
resources = [{ kind = "workspace", id = "*" }]
actions   = ["workspace.read"]
```

Every value in a managed file is a **default** unless its dotted key path is also listed in
`[managed] pins` — a default still fills in when nothing downstream sets it, but a project or user
config may freely change it. A **pinned** key is different: a downstream layer may only make it
*more* restrictive. An attempt to relax a pin (say, a project config setting `[private_net] web =
true` under the example above, which would open egress beyond the pinned host) is refused at load
time with a diagnostic naming the pinned key, never silently merged away or silently allowed.
Making the effective config stricter than the managed floor — narrowing a host list, adding more
entries to `[tools] disable`, leaving a pinned key untouched — is always permitted.

v1's pinnable keys are the security-relevant ones: the `[[policy.grants]]` authorization floor,
`[private_net] web` egress, the `[tools] disable` blocklist, and the `[sandbox]`/`[workspace]
allow_all` confinement knobs. The set is deliberately small and can grow.

**This is an operator control, not a defense against your own machine.** The managed file's
authority comes entirely from filesystem permissions on that one file (e.g. `/etc/flux/config.toml`
owned by root, not writable by the account running `flux`). Anyone who can write that file, or who
owns the machine outright and can patch the `flux` binary, can bypass it — the same honest limit
that applies to every local control described in [Security overview](../security/overview.md). Its
job is stopping an ordinary developer from *accidentally or casually* loosening an audited baseline,
not resisting a privileged attacker on the same host.

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

## Scheduled wake-ups (`[wakeup]`)

The `schedule_wakeup` op lets an agent schedule its own later turn. It is **off by default** and
absent from the operation catalog entirely until enabled — the same off-by-default posture as
`enable_shell`.

```toml
[wakeup]
enabled = true
max_horizon_secs = 86400
max_pending_per_session = 8
```

| `[wakeup]` key | Meaning |
|---|---|
| `enabled` | Surfaces the `schedule_wakeup` op. Default `false`. |
| `max_horizon_secs` | Furthest ahead a single wake-up may be scheduled. Absent means the built-in default. |
| `max_pending_per_session` | Cap on simultaneously pending wake-ups per session. Absent means the built-in default. |

Enabling the table is necessary but not sufficient: registering a wake-up also needs `host.write`
authority, which is approval-gated by the default policy. This table **bounds** an approved
registration; it does not grant one.

Inspect and cancel pending wake-ups with `flux wakeups list` and `flux wakeups cancel` — see the
[CLI reference](../agent/cli.md).

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

Security-relevant booleans only enable on `1`, `true`, `yes`, or `on`; values such as `0` and
`false` stay off. Provider API-key variables are listed under
[Providers and models](../agent/providers.md).

### Paths and workspace

| Variable | Effect |
|---|---|
| `FLUX_STORE_DIR` | Overrides the event-store location; `--store` works by setting it. See [Storage](./storage.md). |
| `FLUX_HOME` | The flux home directory (default `~/.flux`) that `flux usage` resolves its global events store from. It does **not** redirect `--store`/`FLUX_STORE_DIR`. |
| `FLUX_ADD_DIRS` | Extra workspace roots, path-separator delimited; the environment form of `--add-dir`. |
| `FLUX_WORKTREE_DIR` | Where context-local Git worktrees are created. |

### Safety and permissions

| Variable | Effect |
|---|---|
| `FLUX_ALLOW_ALL` | Auto-approves every action — the environment form of `--allow-all-paths`/`--yes`. Do not set it on a shared or non-interactive host without an explicit policy. |
| `FLUX_ENABLE_BASH` | Surfaces the high-risk shell group, like `enable_shell`. |
| `FLUX_ALLOW_PRIVATE_NET` | Grants private-network egress to native web ops. Prefer `[private_net] web`. |
| `FLUX_ALLOW_SOURCE_BUILD` | Permits installing a plugin by building it from source, bypassing the signed-pack channel. See [Plugin trust](../security/plugin-trust.md). |
| `FLUX_SANDBOX` | `off` / `on` / `require`. With `FLUX_SANDBOX_NET`, `FLUX_SANDBOX_WRITABLE`, `FLUX_BWRAP_BIN`, `FLUX_SANDBOX_EXEC_BIN` — see [OS process sandboxing](../security/os-sandbox.md). |
| `FLUX_MANAGED_CONFIG` | Path to the managed config file, overriding the `/etc/flux/config.toml` convention. |

### Server and A2A

| Variable | Effect |
|---|---|
| `FLUX_SERVER_TOKEN` | Shared secret for shared-secret auth mode. |
| `FLUX_SERVER_MAX_BODY_BYTES` | Request-body cap; over it the server answers `413`. A `0` or unparseable value falls back to the default rather than disabling the bound. |
| `FLUX_SERVER_REQUEST_TIMEOUT_SECS` | Response-production timeout; over it the server answers `408`. Same fallback rule. |
| `FLUX_A2A_TOKEN` | Bearer token used when flux calls *out* to another agent. |
| `FLUX_A2A_MAX_INFLIGHT_PER_REALM` | Concurrent in-flight A2A turns permitted per realm. |
| `FLUX_A2A_PUSH_ALLOW_LOCAL` | Permits A2A push notifications to loopback targets. |
| `FLUX_A2A_PUSH_PRIVATE_HOSTS` | Private hosts permitted as A2A push targets. |
| `FLUX_MAX_INFLIGHT_DELIVERIES` | How many program deliveries a running app processes at once (default `64`). Past the bound a delivery **waits** — it is never dropped — so a channel adapter under a storm feels backpressure instead of the app spawning without limit. A `0` or unparseable value falls back to the default. Raise it above your program's fan-out width if journeys deliberately wait on one another. |

See the [HTTP API](../agent/http-api.md) and
[Server authentication & tenancy](../security/server-auth.md).

### Model, context and cost

| Variable | Effect |
|---|---|
| `FLUX_TURN_TOKEN_BUDGET` | Per-turn token budget. |
| `FLUX_COMPACT_CHARS` | Character threshold that triggers history compaction. |
| `FLUX_TOOL_OUTPUT_CAP` | Maximum characters of a single tool result kept in context. |
| `FLUX_CACHE_TAIL` | Tunes the prompt-cache tail boundary. |
| `FLUX_BEDROCK_HAIKU_PROFILE` | Bedrock inference profile used for the small/fast model. |
| `FLUX_CODEX_WS` | Toggles the Codex provider's WebSocket transport. |

### Datasource embeddings

Semantic retrieval over an indexed datasource needs an embeddings endpoint. Without these three,
retrieval falls back to lexical matching.

| Variable | Effect |
|---|---|
| `FLUX_EMBEDDINGS_URL` | Embeddings endpoint URL. |
| `FLUX_EMBEDDINGS_API_KEY` | Its API key. |
| `FLUX_EMBEDDINGS_MODEL` | Embedding model name. |

See [Datasources](../agent/datasources.md).

### Interface and diagnostics

| Variable | Effect |
|---|---|
| `FLUX_VERBOSE` | Verbose output. |
| `FLUX_SHOW_LOOP` | Shows agent-loop steps as they run. |
| `FLUX_NO_SPLASH` | Suppresses the TUI splash screen. |
| `FLUX_BROWSER_BIN` | Chromium executable for `browser.*` ops. |
| `FLUX_TRACE_LOOP` | Traces loop execution. |
| `FLUX_MODEL_TRACE` | Traces model requests and responses. |
| `FLUX_TRANSPORT_DEBUG` | Logs provider transport detail. |
| `FLUX_AUTO_RESURRECT` | Automatically resurrects an interrupted session on restart. |
| `FLUX_VAULT_MOUNT` / `FLUX_VAULT_PREFIX` | HashiCorp Vault mount and path prefix for credential lookup. See [Credentials](../security/credentials.md). |

The diagnostic variables are for troubleshooting, not for normal operation — see
[Troubleshooting](../troubleshooting.md).

## Related docs

- [Safety & approvals](../agent/safety.md) — policy, permissions, and destructive re-checks.
- [Skills & roles](../agent/skills-and-roles.md) — discovery and precedence.
- [Endpoints](../agent/endpoints.md) — weak references and cross-plugin credentials.
- [Credentials & secrets](../security/credentials.md) — token storage and redaction.
- [OS process sandboxing](../security/os-sandbox.md) — the full `[sandbox]` reference.
- [Security overview](../security/overview.md) — the honest-posture summary this doc's managed
  tier and sandbox sections both back.
- `flux doctor` (C-128) — its "config provenance" check answers "why can't I enable this" with the
  effective value and layer of every pinnable key.
