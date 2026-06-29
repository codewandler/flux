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

## How a plugin reaches the outside world

Plugins do **no privileged IO of their own**. Every side effect is a host-capability callback over the
plugin protocol, serviced by flux's guarded host (the same safety boundary as the agent's built-in tools):

- **secret-by-purpose** — the host resolves an auth purpose (e.g. `api_token`) to a value from declared
  env keys; the plugin never reads env directly.
- **http** — method/headers/body, with optional `bearer_purpose` so the host injects
  `Authorization: Bearer <resolved>` (the plugin never sees the raw token on that path).
- **endpoint** — a named base URL resolved from declared env keys.
- **process** — an allow-listed subprocess (e.g. `kubectl`).
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
| `jira` | issue search/show, projects (Cloud REST v3) | Basic (email + API token) | `jira.issue` |
| `confluence` | content search, page show, spaces (Cloud REST) | Basic (email + API token) | `confluence.page` |
| `kubernetes` | namespaces / pods / deployments / logs / events | kubeconfig (via `kubectl`) | `k8s.pod` / `k8s.deployment` |
| `loki` | LogQL query / query_range / labels | bearer (optional) | — |
| `prometheus` | PromQL query / query_range / alerts / targets | bearer (optional) | — |
| `slack` | post / history / channels / users / thread | bearer bot token | `slack.channel` / `slack.user` |

## Installing a plugin into flux

Build the binary, then register a descriptor under `~/.flux/plugins/<name>.toml` (`flux plugin add …`)
pointing at the built `flux-plugin-<name>` binary. `flux` discovers it at startup and projects each
declared operation as a policy-gated tool; the agent's grants decide which ops it may call.
