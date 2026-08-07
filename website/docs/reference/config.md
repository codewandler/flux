---
title: Configuration
description: "Complete .flux/config.toml reference: precedence, permissions, network grants, limits, skills, workspace, endpoints, and server settings."
---

# Configuration

flux works without a config file. User defaults live in `~/.flux/config.toml`; a project can add
`.flux/config.toml` in the directory where flux is launched. flux reads that exact directory and
does not search parent directories for a repository root. An operator can additionally pin an
organization-wide floor ahead of both — see
[Managed configuration tier](#managed-configuration-tier-operator-floor) below.

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

[web]
allowed_secrets = ["GITHUB_TOKEN;to=api.github.com;in=header"]

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
requests_per_minute = 120
max_inflight_per_principal = 4
provider_calls_per_day = 1000
provider_spend_usd_per_day = 25.0

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

Deny rules reach sub-agents: a `task`-delegated child inherits the **denials only** (never the allow
list, which would widen a child past what the operator granted its parent), and they descend through
bounded nested delegation. A sub-agent runs non-interactively, so a subject that merely *prompts* for
the top-level agent is auto-approved for the child — only a deny rule refuses it there.

For `flux app run`, these host rules are evaluated **inside** any `permissions` ceiling declared by
the `.flux` program and its owning agent. A local deny still wins; a local allow can approve a scoped
invocation but cannot restore an operation the app source removed.

`[[policy.grants]]` adds fine-grained authorization grants to the built-in policy floor. Permission
rules cannot widen past that floor, and destructive operations always re-fire the approval gate.
Interactive runs prompt; `--yes` answers every approval gate reached by an admitted operation “yes,”
including destructive ones, but does not widen the policy or an app/agent ceiling. See [Safety &
approvals](../agent/safety.md).

## Private-network egress

DNS is resolved before a request and private, loopback, link-local, unique-local, and internal hosts
are refused by default.

- `[private_net] web` covers the entire native web family: `http.request`, `web.fetch`, and
  `browser.*`. Use a host list, or `true` for any private address.
- `[private_net.plugins]` is keyed by plugin manifest name. The host intersects the operator grant
  with hosts declared by that plugin.
- `[private_net.endpoints]` is keyed by `"<plugin>:<endpoint-name>"` and merges with the owning
  plugin's grant.

The former `[private_net] web_fetch = …` key is gone, and `[private_net]` rejects unknown keys — an
old config still carrying it fails to load with `unknown field \`web_fetch\`` rather than starting
with the grant quietly dropped. Migrate it to `web`.

For a one-off invocation, global `--allow-private-net` temporarily opens native web and outbound
fleet worker calls to every private range, and supplies the operator side of plugin grants:

```bash
flux --allow-private-net plugin call gitlab gitlab.test
```

The plugin still cannot use a host absent from its manifest. Native web has no manifest intersection,
so the flag also admits cloud-metadata addresses for that run; prefer scoped config for recurring
access. Every admitted private request is audited.

## HTTP secret allowlist

`http.request` resolves `{"$secret": "NAME"}` markers in header and structured-query values only
for names an operator allowlisted. The default is deny-all. Configure the list under `[web]`:

```toml
[web]
allowed_secrets = [
  "GITHUB_TOKEN;to=api.github.com;in=header",
  "REPORT_TOKEN;to=reports.example;by=alice;in=query",
]
```

A bare name such as `"GITHUB_TOKEN"` keeps the pre-scoping behavior: it may go to any destination
the web egress guard otherwise permits, on behalf of any principal, in a header or query parameter.
That compatibility form is intentionally unscoped. Scope parameters narrow it:

- `to=` accepts an exact host, `*.suffix` (requiring a real label boundary), or `*`. It is checked
  only after the egress guard resolves and vets the address, and every redirect hop is checked again.
- `by=` matches the principal frozen into the turn identity. A turn with no resolved principal does
  not satisfy a principal-scoped entry.
- `in=header` or `in=query` limits placement. Query credentials are the broader exposure because
  URLs are commonly retained by proxies and access logs. `$secret` substitution is not supported in
  request bodies.

Repeat an entry to allow multiple combinations. User and project lists merge. An explicit empty list
is deny-all and suppresses the environment fallback; if `[web] allowed_secrets` is absent,
`FLUX_WEB_SECRET_ALLOW` remains the equivalent comma- or whitespace-separated environment form.
Malformed scoped entries refuse every use under that name rather than falling back to unscoped.

## Resource limits

`[limits]` bounds two different things: what a run **spends** (`turn_token_budget`) and what it
**uses** (everything below it). All are off unless set.

| `[limits]` key | Default | Meaning |
|---|---:|---|
| `turn_token_budget` | off | Stop consulting models after cumulative turn usage crosses this ceiling. `--turn-budget`, then `FLUX_TURN_TOKEN_BUDGET`, override it. |
| `max_concurrent_tool_calls` | off | How many tool calls one agent may have executing at once. `0` is read as `1`. A call arriving at a saturated agent queues, then is refused with a message naming the limit — never truncated, never silently dropped. |
| `max_live_agents` | off | How many agents may be live across the whole delegated tree, **including the root**. `1` means no delegation; `0` is read as `1`. A spawn over the ceiling is refused immediately, never queued. |
| `tool_call_queue_timeout_ms` | `30000` | How long a queued tool call waits for a slot before that refusal. Meaningful only alongside `max_concurrent_tool_calls`. **Not clamped:** there is no "wait forever" sentinel, but an absurd value is honoured as written — `u64::MAX` ms is a ~585-million-year wait, i.e. a hang you chose. |
| `max_retained_result_bytes` | off | How many bytes of tool results one agent keeps in its deterministic op cache. Reaching it evicts; a miss just re-runs the op, so this never truncates a result the model sees. |
| `max_evidence_payload_bytes` | off | How many bytes of observation payload one agent's in-memory evidence log retains. Reaching it elides the *oldest* payloads behind a marker — no observation is ever dropped, and counts, order, kind and phase are preserved. Payloads from completed turns remain in full in the session event store. |

The execution and retention ceilings are per agent: a `task`-delegated child inherits the same
numbers with separate budgets. `max_live_agents` is different: its census is shared across the root
and every transitive child. Setting both concurrency and census ceilings therefore bounds the whole
tree at `max_concurrent_tool_calls × max_live_agents` simultaneous tool calls; without the census,
an unbounded number of agents can each consume the per-agent ceiling. Retained-result and evidence
byte ceilings remain per agent, so their process-wide upper bound is likewise the configured byte
ceiling times `max_live_agents` when both are set.

Project config overrides user config for each `[limits]` scalar. For SDK embeddings, an explicit
`ClientBuilder::resource_limits` value wins exactly as supplied (including a value built from file
config); only an omitted value selects the autonomous preset for an autonomous client or the
unbounded default for a supervised client.

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

**It binds delegated work too.** The list travels into every `task`-spawned sub-agent (and, through
bounded nested delegation, into their children), where it is re-resolved against that child's own
narrowed catalog — so a disabled op is neither advertised to a sub-agent's model nor dispatchable by
it. Before this it stopped at the delegation boundary.

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

`[sandbox]` turns on OS-level confinement (bubblewrap on Linux, Seatbelt on macOS) for ordinary
shell/exec and plugin subprocess paths, as defense-in-depth underneath the safety envelope. It is off
by default unless configured. The CLI selects `require` automatically for the specific
auto-approved and `--serve` forms listed below. A small, documented set of trusted host/browser
paths remains exempt:

```toml
[sandbox]
enabled = true      # turn on OS sandboxing for spawned processes
require = false     # fail closed instead of warn-and-continue when no backend is usable (implies enabled)
network = true       # default is open; the CLI unattended profile defaults closed unless explicitly true
writable = ["../shared-output"]   # extra writable paths beyond the workspace root and toolchain caches
```

| `[sandbox]` key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | Turn on OS sandboxing for spawned processes. The CLI forms listed below select `require` automatically. |
| `require` | `false` | Fail closed (refuse to spawn) instead of warning when no sandbox backend is usable. Implies `enabled`. |
| `network` | unset (open unless the CLI unattended profile applies) | Whether sandboxed processes may reach the network. `false` closes the sandbox's network namespace/profile. The CLI unattended profile requires an explicit `true` to open it. |
| `writable` | `[]` | Extra writable paths, beyond the workspace root, named/Git-worktree roots, `/tmp`/`$TMPDIR`, and the toolchain caches. A leading `~/` expands to the home directory. Missing configured paths are created as directories before launch; `/` is rejected (use the explicit `--allow-all-paths` hatch instead), and a missing path under the masked `/run` is refused rather than created. This is also the key that makes a **host unix socket** reachable — the sandbox masks `/run`, so e.g. `writable = ["/run/user/1000/pulse"]` is *required* for a process that must reach PulseAudio/PipeWire. See [Reaching a host socket on purpose](../security/os-sandbox.md#reaching-a-host-socket-on-purpose). |

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
`--yes` on `run`, `fork`, `record`, `flow run`, or `app run`; `preset --run --yes`; the
auto-approved `review` flow; and `flux app run --serve` all contribute `require` with network closed
unless `[sandbox] network = true` (or `FLUX_SANDBOX_NET` is truthy). `--no-sandbox`/
`FLUX_SANDBOX=off` remains the explicit, prominently warned escape for a deployment that supplies
equivalent isolation in an outer container or VM.
This automatic floor belongs to CLI assembly. An unflagged `flux app run <program>` may still serve
program-declared HTTP/A2A channels, but it is not the `--serve` form. Direct `flux-sdk`/`flux-server`
embedders likewise receive no automatic serving posture: both inherit an injected or environment
posture and otherwise default to sandbox off with process networking open.
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

## Host bindings (`[[host]]`)

Each `[[host]]` table declares a named binding to an execution substrate. `id` and a `backend`
kind (`local`, `sandboxed`, `container`, `kubernetes`, `microvm`, `ssh` or `remote`) are required;
an unknown backend kind is a hard config error, and an unknown key in a `[[host]]` entry is refused
rather than dropped. A `remote` binding needs a credential-free `url`; `credential_ref` is a
location (the same reference forms as endpoints), never a secret value; non-secret `labels` are
optional.
Project declarations override user declarations with the same id. `flux host add`/`rm` edit the
user layer imperatively, and `flux host ls`/`show`/`probe` inspect and verify bindings.

**The vocabulary is ahead of the wiring, deliberately, and says so.** `local`, `sandboxed`,
`microvm`, `ssh` and `remote` resolve to a substrate today. `container` and `kubernetes` are
declarable and will validate, list and render — but nothing wires them yet, so `flux host ls`
reports them as `unwired (selection fails closed)` and selecting one refuses by name instead of
falling back to your own machine. Declaring one ahead of its implementation is legal and useful
for recording intent; just do not expect it to run anything. The same is true of a `microvm`
binding that names no served endpoint yet.

Selecting a binding (`flux --host <name> …`) is granted, never ambient: `grant` lists the surface
classes allowed to select it — `operator` (attended sessions) and/or `unattended` (serving and
`--yes` surfaces) — and the default is deny. The classes are exact: an unattended surface never
inherits an `operator` grant. A granted `remote` binding executes guarded effects on its serving
endpoint; `--remote <url>` keeps working as sugar for an ephemeral, session-only binding.

A granted `sandboxed` binding runs guarded effects on this machine under OS confinement
(bubblewrap or Seatbelt). It is the `--sandbox` posture as a *substrate* rather than a flag, so it
fails closed: on a platform with no usable confinement backend the binding refuses at startup and
names the reason, instead of quietly running unconfined. `flux host probe <id>` reports whether
this machine can serve it, and which backend it would use. An autonomy posture that will not run
unconfined (`bounded-autonomy`, `exploratory`) selects this backend for a named `local` binding —
a posture may tighten a selection, never loosen one.

Four consequences are worth stating plainly before you declare one.

- **A bare `FLUX_SANDBOXED` marker does not satisfy it.** A flux running inside a flux sandbox is
  confined by that outer sandbox and a `sandboxed` binding inherits it — but only where the run's
  own sandbox posture established it (`FLUX_SANDBOX=on|require`, `[sandbox]`, or an unattended
  profile), which is also what makes flux print the auditable "trusting FLUX_SANDBOXED=1" startup
  line. A marker left in the environment with confinement otherwise off is refused by name, so a
  stale one is a clear error rather than a silent claim of confinement.
- **`--no-sandbox` does not disable an explicitly selected `sandboxed` binding.** The flag governs
  the spawn-time sandbox, which it still turns off exactly as before; a binding you selected by
  name is a separate, explicit request, and confinement resolves tightest-wins everywhere in flux.
  To run unconfined, select a different binding (or none) rather than expecting the flag to
  override the selection.
- **Selecting any substrate pins the workspace.** A selected binding — `sandboxed` or `remote` —
  is resolved once at startup, so guarded effects continue against the root it was selected with
  even after a worktree transition (`git_worktree_enter`, `fleet.isolate`) moves the native path.
  With no `--host`, nothing is pinned and the native path follows transitions as it always has.
- **It serves HTTP; browser operations stay hidden.** A `sandboxed` binding makes web requests
  through the same guarded egress path, redirect rules and private-network audit trail an
  unselected run uses — the request is made in this process against this machine's network, which
  is what confinement of *spawned* work already implied. A substrate that genuinely cannot make
  requests (a remote binding, until HTTP rides the wire) still refuses rather than sending from
  the calling process behind your back. `browser.*` and `web.crawl` remain withheld while any
  selection is in force, because they drive a browser and a crawl frontier in this process; use a
  binding-free run for those.

```toml
[[host]]
id = "build-farm"
backend = "remote"
url = "https://farm.internal:8443"
credential_ref = "env/FLUX_REMOTE_SYSTEM_TOKEN"
grant = ["operator"]
labels = { region = "eu" }
```

A granted `microvm` binding is a VM or microVM guest that serves that same remote protocol: the
same authenticated client, the same handshake, the same credential *reference*. Flux never
creates, starts, stops or destroys a guest — the binding consumes an endpoint that already exists.
That endpoint comes to exist through the [VM or microVM guest
profile](../remote-system-deployment.md#vm-or-microvm-profile) — a hardened service unit, an
idempotent install contract and a cloud-init bootstrap in
[`deploy/vm/`](https://github.com/codewandler/flux/tree/main/deploy/vm) — whose daemon binds
`0.0.0.0:8790` inside the guest.

```toml
[[host]]
id = "vm-guest"
backend = "microvm"
url = "https://guest.internal:8790"
credential_ref = "env/FLUX_REMOTE_SYSTEM_TOKEN"
grant = ["operator"]
```

Declared without a `url`, a `microvm` binding is still legal, and honestly **unwired**: `flux host
ls` says so and selection fails closed naming the missing endpoint, because that gap is closed by
deploying the guest rather than by retrying. With one, `flux host probe <id>` reports the
negotiated protocol version and the guest's own substrate identity, marked as remotely reported —
the guest measured itself; this machine did not.

### The `ssh` binding

An `ssh` binding reaches the substrate almost every operator already has: a machine with sshd on
it. **ssh is the bootstrap, not the substrate.** Selecting the binding forwards a local port to the
far machine's loopback, makes sure `flux system serve` is running there, and then rides the ordinary
remote protocol through that forward — same TLS, same bearer token, same version negotiation, same
handshake. Guarded operations are never mapped onto remote shell commands: the far side is still the
flux binary enforcing its own capabilities, which is the whole reason a remote substrate can be
trusted at all.

```toml
[[host]]
id = "devbox"
backend = "ssh"
url = "ssh://build@devbox.internal:22"      # where sshd is; `build` is a login name, not a secret
credential_ref = "env/FLUX_DEVBOX_KEY"      # holds the *path* of the private key to offer
grant = ["operator"]
ssh = { binary = "/usr/local/bin/flux", serve_port = 8790, workspace = "/srv/flux/workspace", cert = "/run/flux-tls/tls.crt", key = "/run/flux-tls/tls.key", ca = "/etc/flux/devbox-ca.pem" }
```

| `ssh` key | Meaning |
|---|---|
| `binary` | The far-side flux binary. Default `flux`, resolved on the far side's `PATH`. |
| `serve_port` | The far-side loopback port the serve binds and the forward lands on. Default `8790`. |
| `workspace` | The far-side workspace root a started serve is given. |
| `cert` / `key` | Far-side TLS material. **Both are required to *start* a serve**; a binding without them may only attach to one you run yourself. |
| `ca` | A **local** PEM whose roots this binding trusts — the same pinning `--remote-ca` does, not a TLS bypass. |
| `known_hosts` | A **local** `known_hosts` file scoping host-key verification to this binding. Verification is strict either way; this only says which record. |
| `server_name` | The name the far side's certificate carries. Default `127.0.0.1`, the address the forward lands on. |
| `token_ref` | Location of the serving endpoint's bearer token. Default `env/FLUX_REMOTE_SYSTEM_TOKEN`. Never a value. |

**What must already exist on the far machine.** Installing flux there is your step, exactly as it is
for a `remote` binding — see [Remote system deployment](../remote-system-deployment.md) and the
shipped `deploy/` artifacts. This binding starts or attaches to what is there; it never installs
anything. Concretely the far machine needs the flux binary at `binary` (or on `PATH`), the TLS
certificate and key at `cert`/`key` if you want flux to start the serve, and a way to obtain its
bearer token: either it already holds it (a systemd `EnvironmentFile`, as `deploy/vm` ships), or its
sshd accepts the variable (`AcceptEnv FLUX_REMOTE_SYSTEM_TOKEN`) so the local side can hand it over
through the ssh channel. The token is never passed as a command-line argument on either machine,
because arguments are visible in a process table to everyone on the box.

**What the tunnel does and does not do.** It carries the connection; it does not authenticate it.
The bearer token still authenticates every request, the certificate is still verified against
`server_name`, and a version mismatch still refuses to pair — a far side reached over ssh is admitted
by exactly the checks a directly addressed one is. And because those answers come from another trust
boundary, an ssh binding is a *non-native* selection like any other: it serves no local HTTP, and
`browser.*` / `web.crawl` are withheld while it is in force.

Four more things worth knowing before you declare one.

- **Nothing prompts, ever.** Host-key checking is strict and `BatchMode` is on, so an unknown or
  changed host key is a named refusal rather than a question — an unattended run has nobody to
  answer one. Password and keyboard-interactive authentication are off, agent forwarding and
  connection multiplexing are off, and `-F none` means neither your `~/.ssh/config` nor the
  system-wide one is consulted. The `[[host]]` entry is the whole declaration — which also means
  **`ProxyJump` is not supported**: a machine reachable only through a jump host cannot be reached
  by an ssh binding today, because no binding field declares one. Point the binding at a directly
  reachable target, or serve that machine and use a `remote` binding.
- **Nothing reaps a far-side serve.** If flux started `flux system serve` on the far machine, it
  keeps running after your session ends — that is deliberate, because the next session attaches to
  it instead of starting another, and because killing a process on someone else's build machine is
  not a thing a client should do implicitly. Stop it the way you stop any other service there.
- **The key is a reference, and it stays a file.** `credential_ref` resolves to the private key's
  *path*; openssh opens it and flux never reads the material. An `ssh://user:pass@host` url is
  refused — an ssh binding authenticates by key.
- **Every failure names its piece.** No sshd reachable, a host key that is not the one on record, a
  key sshd declined, no flux binary at the declared path, nothing serving and no certificate to
  start one with, a refused handshake — each is a distinct message. Nothing ever falls back to
  running the effect on your own machine.
- **Two sessions do not fight.** Starting a serve is idempotent because the far side's bind address
  is the arbiter: a second session that tries loses the bind, its attempt exits, and it attaches to
  the serve that won. `flux host probe` never starts one at all — a probe is side-effect-free, so
  against a far side with nothing serving it reports that rather than launching a process on your
  build machine.

`[exchange] host = "<binding>"` names the `[[host]]` entry serving the Exchange catalogue — the
declared home for what the transitional `FLUX_EXCHANGE_URL`/token environment pair configures.
The pair keeps working and wins while present; the named binding's `url` is the origin and its
`credential_ref` locates the service-account token.

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
| `requests_per_minute` | Authenticated protected-route requests (reads and work) admitted per principal/auth realm each minute; default `120`. Health and discovery routes are exempt. |
| `max_inflight_per_principal` | Live REST, webhook, and A2A turns per principal/auth realm; default `4`. |
| `provider_calls_per_day` | Completed provider-call circuit-breaker threshold per principal/auth realm in each 24-hour process window; default `1000`. Already-admitted turns can overshoot within the live-work bound. |
| `provider_spend_usd_per_day` | Completed priced-spend circuit-breaker threshold per principal/auth realm in each 24-hour process window; default `$25`. Already-admitted turns can overshoot within the live-work bound. |
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
| `FLUX_SESSION_ID` | Explicit session selected by session-scoped `flux board` operations when no live turn supplies one. |
| `FLUX_EXCHANGE_URL` | Absolute origin of the operator-selected Exchange deployment. Set it together with `FLUX_EXCHANGE_SERVICE_ACCOUNT_TOKEN`; Flux pins this origin and never accepts it as model/tool input. HTTPS is required except when every resolved address is loopback for local development. |
| `FLUX_EXCHANGE_SERVICE_ACCOUNT_TOKEN` | Transitional C-503 compatibility bearer for one Exchange Service Account, not the managed Linux-local Milestone 1 bootstrap. It is registered for redaction at startup and is never accepted in argv, config files, URLs, or tool input. C-509 replaces this environment path only for managed Linux-local direct handoff; independently provisioned remote Exchange use retains it on every Flux target until secure remote provisioning is separately contracted. If either Exchange variable is absent, Exchange operations stay disabled while core Flux remains available. |
| `FLUX_EVAL_BINARY` | Trusted-host path to the flux executable evaluated by `eval_run` and `flux eval` (default: the running executable). This selector is intentionally not accepted as an `eval_run` tool argument; relative paths resolve against the host workspace before the child enters its temporary task directory. |
| `FLUX_TERMINAL_BENCH_BINARY` | Trusted-host command/path for the terminal-bench driver used by the `terminal-bench` eval adapter (default: `tb` from the host `PATH`). Model-facing eval input cannot override it. |
| `FLUX_TERMINAL_BENCH_DATASET` | Trusted-host terminal-bench dataset selector (default: `terminal-bench-core`). It is host-owned because selecting a dataset may fetch and execute benchmark material. |
| `FLUX_TERMINAL_BENCH_REBUILD` | Truthy values allow terminal-bench preparation to run the fixed host-side musl `cargo build` before evaluation (default: off). Model-facing `eval_run` input cannot enable it. This operator-selected preparation step is deliberately unsandboxed; task runners and evaluated child agents use their ordinary sandbox posture. |

### Safety and permissions

| Variable | Effect |
|---|---|
| `FLUX_ALLOW_ALL` | Lifts filesystem read and write confinement, like `--allow-all-paths` or `[workspace] allow_all = true`. It does **not** approve actions, change network policy, or act as an environment form of `--yes`. |
| `FLUX_ENABLE_BASH` | Surfaces the high-risk shell group, like `enable_shell`. |
| `FLUX_ALLOW_PRIVATE_NET` | Blanket private-network override for native web ops and outbound fleet worker calls; also supplies the operator side of plugin grants (the plugin's manifest declaration still bounds it). Prefer scoped `[private_net]` grants for recurring access. |
| `FLUX_WEB_SECRET_ALLOW` | Comma- or whitespace-separated `http.request` secret entries, using the same `NAME;to=host;by=principal;in=header|query` grammar as `[web] allowed_secrets`. Used only when the config key is absent; unset/empty means deny-all. |
| `FLUX_ALLOW_SOURCE_BUILD` | Permits installing a plugin by building it from source, bypassing the signed-pack channel. See [Plugin trust](../security/plugin-trust.md). |
| `FLUX_SANDBOX` | `off` / `on` / `require`. With `FLUX_SANDBOX_NET`, `FLUX_SANDBOX_WRITABLE`, `FLUX_BWRAP_BIN`, `FLUX_SANDBOX_EXEC_BIN` — see [OS process sandboxing](../security/os-sandbox.md). |
| `FLUX_MANAGED_CONFIG` | Path to the managed config file, overriding the `/etc/flux/config.toml` convention. |

### Server and A2A

| Variable | Effect |
|---|---|
| `FLUX_SERVER_TOKEN` | Shared secret for shared-secret auth mode. |
| `FLUX_SERVER_MAX_BODY_BYTES` | Request-body cap; over it the server answers `413`. A `0` or unparseable value falls back to the default rather than disabling the bound. |
| `FLUX_SERVER_REQUEST_TIMEOUT_SECS` | Response-production timeout; over it the server answers `408`. Same fallback rule. |
| `FLUX_SERVER_REQUESTS_PER_MINUTE` | Overrides the per-principal/auth-realm protected-request rate. |
| `FLUX_SERVER_MAX_INFLIGHT_PER_PRINCIPAL` | Overrides the cross-surface live-work cap. |
| `FLUX_SERVER_PROVIDER_CALLS_PER_DAY` | Overrides the completed provider-call circuit-breaker threshold. |
| `FLUX_SERVER_PROVIDER_SPEND_USD_PER_DAY` | Overrides the completed priced-spend circuit-breaker threshold. |
| `FLUX_APPROVAL_TIMEOUT_SECS` | How long a served agent under `--remote-approval` waits for a human decision at `/approvals` before **denying** the effect (default `120`, maximum `3600`). There is deliberately no "wait forever" value — an unbounded wait is a wedged turn, not a decision. Larger values are capped; an unparseable value falls back to the default. |
| `FLUX_REMOTE_SYSTEM_TOKEN` | Default bearer token used by `flux system serve` and agent `--remote` mode. Use `--token-env` / `--remote-token-env` to name a different environment variable; bearer values are never accepted as URL/query literals. |
| `FLUX_A2A_TOKEN` | Bearer token used when flux calls *out* to another agent. |
| `FLUX_A2A_MAX_INFLIGHT_PER_REALM` | Concurrent in-flight A2A turns permitted per realm. |
| `FLUX_A2A_PUSH_ALLOW_LOCAL` | Permits A2A push notifications to loopback targets. |
| `FLUX_A2A_PUSH_PRIVATE_HOSTS` | Private hosts permitted as A2A push targets. |
| `FLUX_MAX_INFLIGHT_DELIVERIES` | How many program deliveries a running app processes at once (default `64`). Past the bound a delivery **waits** — it is never dropped — so a channel adapter under a storm feels backpressure instead of the app spawning without limit. A `0` or unparseable value falls back to the default. Raise it above your program's fan-out width if journeys deliberately wait on one another. |

See the [HTTP API](../agent/http-api.md) and
[Server authentication & tenancy](../security/server-auth.md).

### Model, context and cost

What these three actually bound, and what flux deliberately does not manage, is explained in
[Context management](../agent/context-management.md).

| Variable | Effect |
|---|---|
| `FLUX_TURN_TOKEN_BUDGET` | Per-turn token budget. |
| `FLUX_COMPACT_CHARS` | Character threshold (of serialized history) that triggers history compaction; `0` disables it. Default `48000`. Not a fraction of the model's context window — the same count applies to every model. |
| `FLUX_TOOL_OUTPUT_CAP` | Maximum characters of a single tool result kept in context (default `20000`; `0` disables trimming). |
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
| `FLUX_VAULT_MOUNT` / `FLUX_VAULT_PREFIX` | Optional constructor inputs read by `VaultCredentialStore::from_env()` in an embedding host. They do not switch the stock CLI/server credential backend. See [Credentials](../security/credentials.md). |

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
- `flux doctor` — its "config provenance" check answers "why can't I enable this" with the effective
  value and layer of every pinnable key.
