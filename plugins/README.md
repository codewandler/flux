# flux integration plugins (D-08)

A nested cargo workspace of **native flux plugins** — subprocess binaries that speak flux's plugin
protocol (`flux-plugin`). It is **excluded from the root flux workspace** (see `../Cargo.toml`) so the
heavy/vendor surface here never enters the main `flux` gate. Build and test from this directory:

```
cd plugins
cargo build            # all plugin binaries + host-kit
cargo test             # hermetic — every op is unit-tested against a MockHost (no network/subprocess)
cargo clippy --all-targets -- -D warnings
```

**Writing or deepening a plugin? Read [AUTHORING.md](AUTHORING.md) first** — the canonical guide:
lifecycle (install → configure → call), the host-does-all-IO invariant, the full host-capability set,
and the authoring rules.

## How a plugin reaches the outside world

Plugins do **no privileged IO of their own**. Every side effect is a host-capability callback over the
plugin protocol, serviced by flux's guarded host (the same safety boundary as the agent's built-in tools):

- **secret-by-purpose** — the host resolves an auth purpose (e.g. `api_token`) to a value from declared
  env keys; the plugin never reads env directly (its process is launched **env-cleared**).
- **http** — method/headers/body; the host injects auth per the declared `AuthScheme`
  (Bearer/Basic/Header/Query) so the plugin never sees the raw token; binary bodies via
  `body_b64`/`response_binary` (byte-exact up/download).
- **endpoint** — a named base URL resolved from declared env keys.
- **process** — an allow-listed subprocess (e.g. `kubectl`), run to completion or as a long-lived
  host-managed background child (`process.spawn`/`read`/`status`/`kill`, e.g. `kubectl port-forward`).
- **conn** — a guarded raw TCP/Unix byte stream (`conn.dial`/`read`/`write`/`close`) for non-HTTP
  protocols (SQL wire, the Docker socket, AMI).
- **blob** — a scratch blob store (`blob.put`/`get`/`info`) so file up/downloads aren't inlined as base64.
- **datasource records** — a plugin contributes `flux-datasource` `Record`s that become searchable
  knowledge in the D-07 index (via the host's `DatasourceHostCaps` bridge).

A plugin **declares** in its manifest exactly the capabilities (secrets / process / http), auth methods,
endpoints, and datasources it uses; the host denies anything undeclared (deny-by-default).

## `host-kit`

The shared SDK (`host-kit/`) wraps the guest protocol so a plugin is mostly "declare ops + implement each
against a vendor API": a typed [`Host`] (`secret`/`endpoint`/`http`/`get_json`/`send_json`/`run`/
`contribute`), a `PluginBuilder` (collect a manifest + op closures, then `serve()`), `read_op`/`write_op`
spec helpers, and a `MockHost` for hermetic unit tests. See `gitlab/src/main.rs` for the reference shape.

## The pack

| Plugin | Surface | Auth | Datasource records |
|--------|---------|------|--------------------|
| `websearch` | Tavily (+ DuckDuckGo fallback) | Tavily key (optional) | `web.result` |
| `gitlab` | projects / MRs / issues / pipelines (REST v4) | `PRIVATE-TOKEN` | `gitlab.project` / `merge_request` / `issue` |
| `jira` | issues / transitions / comments / attachments (Cloud REST v3) | Bearer `api_token` via `cloud_id` gateway (Basic fallback) | `jira.issue` |
| `confluence` | pages / comments / attachments / spaces (Cloud REST) | Bearer `api_token` via `cloud_id` gateway (Basic fallback) | `confluence.page` |
| `kubernetes` | nodes / pods / deployments / logs / events / port-forward | kubeconfig (via `kubectl`) | `k8s.pod` / `k8s.deployment` |
| `loki` | LogQL query / query_range / metric / labels | Basic + `X-Scope-OrgID` tenant (optional) | — |
| `prometheus` | PromQL query / query_range / series / targets / rules / alerts | bearer (optional) | — |
| `slack` | messages / files / reactions / bookmarks / mentions / users | bearer bot token | `slack.channel` / `slack.user` |
| `alertmanager` | alerts / silences | optional Basic | `alertmanager.alert` |
| `grafana` | datasources / dashboards / annotations / Loki+Prometheus+Tempo+Alertmanager proxy ops | Bearer token (Basic fallback) | `grafana.dashboard` / `grafana.annotation` |
| `opsgenie` | alerts / notes / on-call / schedules | `GenieKey` API key | `opsgenie.alert` |
| `huggingface` | Hub model/dataset/space catalog + router chat/embed | bearer token (optional for public Hub reads) | `huggingface.model` / `dataset` / `space` |
| `aws` | STS / EC2 / EKS / RDS / S3 / CloudWatch read ops | AWS access key env via host-managed CLI | — |
| `docker` | core Docker Engine container/image/network/volume/system lifecycle | local Docker Unix socket | `docker.container` / `image` / `network` / `volume` |
| `sql` | PostgreSQL read-only query + database/table/index introspection | SQL DSN + username/password | `sql.query_result` |
| `asterisk` | AMI ping / channels / peers / queues / device states / originate / hangup / command | AMI username/secret | — |
| `homer` | SIP search / calls / QoS / PCAP export / aliases | Homer username/password JWT login | `homer.message` / `call` / `alias` |

The **Surface** column is indicative; each plugin now carries its fluxplane counterpart's full op set
(D-14 through D-17). Run `flux plugin skill` for the live per-plugin op reference, or see the
[parity matrix](../docs/designs/fluxplane-plugins-parity.md).

## Installing + invoking plugins

Build the binaries, then register them as descriptors under `~/.flux/plugins/<name>.toml`:

```
cd plugins && cargo build --release      # → plugins/target/release/flux-plugin-<name>
flux plugin install                      # register every built flux-plugin-* binary (one-shot)
#  …or one at a time:
flux plugin add gitlab  /abs/path/to/flux-plugin-gitlab
flux plugin ls                           # list installed plugins
```

`flux` discovers them at startup (`flux run`, `flux app run`) and projects each declared operation as a
policy-gated tool; the agent's grants decide which ops it may call.

**Invoke one op directly** (debugging / scripting / the smoke), without an agent:

```
flux plugin call gitlab gitlab.project.list '{}'
flux plugin call websearch websearch.search '{"query":"warm transfer"}'
```

`flux plugin call` spawns the plugin and drives the op through the same guarded host + datasource bridge
the agent uses. A live, env-gated smoke over the whole pack is `scripts/smoke-plugins.sh` (skips an
integration when its key is absent).
