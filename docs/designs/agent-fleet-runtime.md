# Agent fleet runtime — addressing, lifecycle and discovery for a fleet of agents

Story: [A-119](../stories/A-119-agent-fleet-runtime-epic.md) · Pillar: Agent · Status: design

## The two questions, and the tree's honest answers

The [fleet coordinator](fleet-coordinator.md) design assumes workers exist at known URLs. It never
says who starts them or how the coordinator finds them. Both answers today are the same word:
**nobody**.

### Who starts an A2A-reachable agent?

A human, in a shell. There is exactly one OS process — the `flux` CLI you typed a command into — and
every A2A-reachable agent is a long-lived `Arc<FlowEngine>` inside its Tokio runtime. Two shipped
paths bind a listener:

- `flux app run --serve <addr>` (`crates/flux-cli/src/app_cmd.rs:311` → `:373` →
  `flux_server::serve`, `crates/flux-server/src/lib.rs:447`), and
- a Program declaring `channel … kind = "a2a"`, which mounts the same single-agent router
  (`crates/flux-channels/src/adapters/a2a.rs:138` → `:151`).

`flux serve` was removed by D-23. The resolver-keyed multi-agent mount (D-63, `router_multi` /
`StaticResolver`, `crates/flux-server/src/lib.rs:903`) is implemented and tested but has **no
production caller** — it is an embedder-only library surface.

What flux supervises is *tasks inside* that process: cancellation tokens, a `JoinSet`, turn gates,
TTL sweeps. What it supervises about the **process** is nothing. `crates/flux-channels/src/host.rs:63-78`
is the whole supervision story, and a fatal channel error tears the entire process down with no
restart and no backoff. `GET /health` exists and nothing consumes it. There is no Dockerfile, no
systemd unit, no k8s manifest, no `--daemon`, and flux never spawns `flux`.

### How does an agent learn another agent exists?

It doesn't. Every path in the tree *addresses* an agent whose identity you already supplied:

- the **A2A agent card** (`crates/flux-server/src/a2a.rs:515`) answers "what is at this URL",
  never "which agents exist" — and there is no index route on either mount;
- `flux a2a <URL>` takes the URL as a required positional;
- **roles** (`.flux/agents/*.md`, `RoleRegistry` at `crates/flux-agent/src/role.rs:146`) are a
  local persona catalog — and the `task` op doesn't even expose the role list to the model
  (`crates/flux-orchestrate/src/lib.rs:1070`: the schema is `{role: String, task: String}`, no enum);
- `agent/getAuthenticatedExtendedCard` returns `-32004`; there is no MCP code at all (D-193 is
  backlog with zero lines).

**But one real discovery mechanism already exists, for services rather than agents**: the endpoint
broker (D-25…D-32, `crates/flux-capabilities/src/endpoint/broker.rs`). A consumer asks the host
"which endpoints exist for product X"; the host fans out to provider plugins (`discover` at `:426`,
`providers_for` at `:130`, `ProviderInvoker` at `:146`) and returns weak refs — url, `product`,
`protocol`, `credential_ref`, `labels` — **never a secret** (`CredentialReader` at `:274`). Static
entries live in `~/.flux/endpoints.toml` as `[[endpoint.static]]`
(`crates/flux-config/src/lib.rs:125`), and the model-facing ops are `endpoint.discover` / `.list` /
`.info` / `.select` / `.import`.

That is the shape this design reuses rather than reinvents.

## Two axes, deliberately not conflated

A `proc://claude` and an `a2a://gpu-box:1234/joe` differ on two **independent** questions:

| Axis | Question | Values |
|---|---|---|
| **Runtime** | who starts, stops and observes the process? | `external` · `proc` · `docker` · `k8s` |
| **Transport** | how do you hold a conversation with it? | `a2a` (HTTP JSON-RPC) · `ndjson` (stdio) |

Conflating them is the mistake that makes the abstraction leak: a k8s pod and a local fork are the
same *conversation* and a different *lifecycle*; a forked `flux` and a forked `claude` are the same
*lifecycle* and a different *conversation*.

### The address: scheme picks the runtime, transport is a defaulted property

```
a2a://gpu-box:1234/joe            # external runtime, a2a transport
https://acme.dev/agents/joe       # external runtime, a2a transport, card at /.well-known
proc://flux?program=worker.flux   # proc runtime,     a2a transport on a local port
proc://claude?proto=ndjson        # proc runtime,     ndjson transport over stdio
proc://codex?proto=ndjson
docker://ghcr.io/acme/worker:1.2  # docker runtime,   a2a transport
k8s://prod/deploy/flux-worker     # k8s runtime,      a2a transport
```

- **Scheme = runtime.** `a2a` and `https`/`http` both mean *external* — something already running
  that we never start or stop; the difference is only whether the URL is the RPC endpoint or a base
  to fetch the card from.
- **Transport is inferred from the scheme and the target**, and overridable with `?proto=`. Default
  is `a2a` everywhere; `proc://claude` and `proc://codex` default to `ndjson` because they cannot
  serve A2A.
- **Query = runtime params** (`program=`, `image=`, `env=`, `replicas=`, …), parsed per runtime and
  rejected as unknown otherwise — no silently-ignored keys.
- The address is the coordinator's **primary key** for an agent, and the string it writes onto a
  board item's `runner` field.

**Safety:** any address with a network authority resolves through the same guarded-origin check as
every other egress (`guard_url_scoped`), and a `proc://`/`docker://` address is a **process-spawn
authority**, not a URL — it must declare accurate `effects` and a concrete `permission_subject`, and
it is exactly as dangerous as `bash`. Starting an agent is a privileged act; §"Safety envelope"
below makes that explicit rather than incidental.

## The `AgentRuntime` port

A new **L5 crate, `flux-fleet`**, classified in `crates/flux-codegate/src/lib.rs`'s `layer()` map.
It is the one home for "coordination of agents as processes", and its layer lets it reach
`flux-system` (L2, guarded spawn), `flux-plugin` (L4, docker/k8s providers) and `flux-a2a` (L1,
the transport client).

```rust
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    /// The URI scheme this runtime owns (`proc`, `docker`, `k8s`, `a2a`/`https`).
    fn scheme(&self) -> &str;
    /// Concrete authority this runtime needs — process spawn, socket, or network origin.
    fn access(&self) -> Vec<RuntimeAccess>;

    async fn start(&self, ctx: &ToolContext, spec: &AgentSpec) -> Result<AgentHandle>;
    async fn stop(&self, ctx: &ToolContext, h: &AgentHandle, grace: Duration) -> Result<()>;
    async fn status(&self, ctx: &ToolContext, h: &AgentHandle) -> Result<AgentStatus>;
    /// Where to talk to it, once it is ready — the transport's connect info.
    async fn endpoint(&self, ctx: &ToolContext, h: &AgentHandle) -> Result<AgentEndpoint>;
}

pub enum AgentStatus { Starting, Ready, Busy, Unreachable, Exited { code: Option<i32> } }
```

Backends, in shipping order: `ExternalRuntime` (start/stop are refusals, `status` is a card fetch —
this is the one that already works today and it is the contract suite's baseline), `ProcessRuntime`,
`DockerRuntime`, `KubernetesRuntime`.

**Readiness is `status`, not `start` returning.** `start` returns a handle; the agent is usable only
when `status` reports `Ready`, which for the `a2a` transport means the agent card answered. This is
what makes `docker://` and `k8s://` honest — a container that is scheduled is not an agent that can
take a turn.

### `ProcessRuntime` reuses two existing patterns, and invents neither

- **Guarded spawn**: `flux-system`'s `build_command` (`crates/flux-system/src/lib.rs:1938`, async at
  `:1983`) plus `sandbox::configure` (`sandbox.rs:281`) is the same path the `bash` op goes through.
  A forked agent inherits the sandbox posture; it does not get a new, weaker one.
- **Stdio child supervision**: `flux-plugin`'s host (`crates/flux-plugin/src/host/loading.rs:12`)
  already runs framed-NDJSON child processes with lifecycle. The `ndjson` transport is the same
  shape one layer up, and C-160's line vocabulary
  ([ndjson-agent-protocol.md](ndjson-agent-protocol.md)) is the wire.

For `proc://flux`, the child is `flux app run --serve 127.0.0.1:0` on an ephemeral port; the parent
learns the port from the child's first stdout line and then speaks ordinary A2A to loopback.

## Discovery: the endpoint broker, with agents as a product

**No second discovery mechanism.** Agents become a `product` in the broker that already exists:

- **Static roster** — `[[endpoint.static]]` entries in `~/.flux/endpoints.toml` with
  `product = "agent"`, the address in `url`, and `labels` carrying the fleet facts the coordinator
  reasons over (`repo`, `tier`, `cluster`). `credential_ref` already means "a location, never a
  value", which is exactly right for a worker's bearer token.
- **Dynamic discovery** — provider plugins answer `endpoint.discover { product: "agent" }`. The
  existing kubernetes plugin (D-28) can enumerate live pods as agents; a docker provider can
  enumerate containers. **A fleet that grows a pod becomes visible with no config edit** — that is
  the whole reason for reusing the broker instead of a static file.
- **Multiple clusters** fall out of `labels`: a cluster is a label value, and `fleet.list` filters on
  it. There is no cluster object to model.

`fleet.list` is therefore a thin projection over `endpoint.discover`/`.list`, not a new store.

## The roles/fleet unification

Sub-agent roles and remote agents are today **disjoint namespaces**: a role cannot be remote
(`Role`, `crates/flux-agent/src/role.rs:16`, has no address field), an `AgentDecl` cannot be remote,
and no code bridges them.

`Role` gains one optional field:

```yaml
---
name: heavy-worker
address: k8s://prod/deploy/flux-worker    # absent ⇒ in-process, exactly as today
tools: [read, write, bash]
---
```

`task(role)` then routes: **absent address → `LocalSpawner`** (unchanged, in-process,
`crates/flux-orchestrate/src/lib.rs:276`); **present → `A2aSpawner`** (A-116). One delegation
vocabulary, local or remote.

**The hard part, stated plainly:** `cap_scope` is enforced today by *constructing the child's
narrowed `ToolRegistry` in-process* (`lib.rs:290`, `:310`, with `task` stripped at leaf depth
`:325`). Across the wire, that construction happens on the **worker**, which is a different trust
domain. So a remote role's `tools` list is a **request**, not an enforcement. The design's position:
the worker's own policy is authoritative, the requested scope travels as a declared intent, and the
divergence is surfaced — never silently trusted. A remote role whose worker cannot prove its scope
is a policy decision at the coordinator, not a fiction maintained in the type system.

## Safety envelope

Three genuinely new authorities, each of which must be gated rather than assumed:

1. **Process spawn as an agent op.** `fleet.start` on a `proc://` address executes a binary. It is
   `bash`-class and must be surfaced as such — accurate `effects`, concrete
   `permission_subjects` (the resolved program path / image ref / workload, never `*`), and no
   ambient enablement. Mirror the `bash` op's opt-in posture rather than shipping it always-on.
2. **A model-supplied address is an SSRF and RCE surface.** Every address that reaches a runtime is
   guarded: network authorities through `guard_url_scoped`, `proc://` targets through an allowlist
   of agent kinds (`flux`, `claude`, `codex`) rather than an arbitrary command line.
3. **Card-advertised endpoints are adopted.** `A2aClient::adopt_endpoint`
   (`crates/flux-a2a/src/client.rs:115`) takes the URL the card advertises, only rewriting it when a
   card advertises loopback from a non-loopback base — an arbitrary cross-origin advertised URL is
   adopted as-is. For a *discovered* fleet this becomes a redirection surface, and the fleet path
   must re-guard the adopted origin.

## What this does not attempt

- **No process manager.** `fleet.start` starts; it does not keep alive. Restart policy belongs to
  the runtime backend that already has one (k8s, docker), and for `proc://` the honest answer is
  "the coordinator's sweep notices `Exited` and re-dispatches the work item" — which is exactly the
  crash-recovery story [fleet-coordinator.md §5](fleet-coordinator.md) already relies on.
- **No autoscaling, no load balancing, no replicas.** `fleet.list` + labels + the board's queue is
  the placement policy; anything smarter is a later story with a real workload behind it.
- **No change to the per-engine turn gate.** One agent still serves one turn at a time
  (`crates/flux-flow/src/engine.rs:170`); horizontal capacity comes from more agents, which is now
  something the coordinator can actually create.

## Two facts a deployment must know

- **Program-mode servers are storeless.** `flux app run <program>` builds `EventStore::in_memory()`
  (`crates/flux-cli/src/app_cmd.rs:509`), so a restarted worker loses every session and answers
  `tasks/get` with not-found. `flux app run --serve` (no program) persists to `~/.flux/events.db`.
  A fleet worker started by `ProcessRuntime` must be started on a persistent store or the
  coordinator's sweep cannot tell "finished" from "never existed".
- **An in-flight task at restart reports `working` forever** (`crates/flux-server/src/a2a.rs:1195-1199`)
  until the TTL sweep — which is itself lazy, running only at the next mint. The coordinator must
  treat `working` + an `Exited` runtime status as failure, and not wait on the task's own report.

## Stories

| ID | Story | Notes |
|---|---|---|
| [A-119](../stories/A-119-agent-fleet-runtime-epic.md) | **Epic** — agent fleet runtime | |
| [A-120](../stories/A-120-flux-fleet-crate-and-agent-address.md) | The `flux-fleet` crate + `AgentAddress` | new crate ⇒ `flux-codegate` layer map |
| [A-121](../stories/A-121-agent-runtime-port.md) | `AgentRuntime` port + `ExternalRuntime` + contract suite | |
| [A-122](../stories/A-122-process-runtime.md) | `ProcessRuntime` over `flux-system` guarded spawn | ⚠ process-spawn authority |
| [A-123](../stories/A-123-ndjson-transport.md) | NDJSON/stdio transport — `proc://claude`, `proc://codex` | needs C-160 |
| [A-124](../stories/A-124-docker-runtime.md) | `DockerRuntime` | |
| [A-125](../stories/A-125-kubernetes-runtime.md) | `KubernetesRuntime` over the existing k8s plugin | |
| [A-126](../stories/A-126-fleet-discovery-over-endpoint-broker.md) | Agents as an endpoint-broker product; `fleet.list` | |
| [A-127](../stories/A-127-roles-carry-an-address.md) | Roles carry an address; `task` routes local/remote | ⚠ cap_scope across trust domains |
| [A-128](../stories/A-128-fleet-lifecycle-ops-and-monitor.md) | `fleet.start`/`.stop`/`.status` + the monitor journey | |

Order: A-120 → A-121 → {A-122, A-126} → {A-123, A-124, A-125} → A-127 → A-128.
