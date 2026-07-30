---
title: Plugin authoring
description: "Author a capability-scoped plugin: the manifest, the process lifecycle, the IO contract, and the host-kit SDK."
---

# Plugin authoring

A plugin is a **subprocess binary** that speaks flux's framed `flux.plugin.v1` NDJSON protocol over
stdin/stdout. It advertises a **manifest** — a name, a set of operations, and the capabilities, auth
purposes, and endpoints it needs — and implements each operation. flux projects every declared
operation as a policy-gated tool, so a plugin op traverses the same
[safety envelope](../agent/safety.md) as a built-in: authorization, approval, guarded IO. There are
no bypass paths.

"Native" here means a Rust subprocess built on flux's own `host-kit` crate. Not an MCP bridge, not a
wrapper that shells out to a vendor CLI of its own accord.

This page is the public contract. The in-repo source guide is
[plugins/AUTHORING.md](https://github.com/codewandler/flux/blob/main/plugins/AUTHORING.md), which
carries the per-op recipe, the typed-migration status, and the build/release gate.

## The one rule: the host does all privileged IO

**A plugin performs no privileged IO of its own.** Every side effect — an HTTP request, a socket, a
subprocess, a host file read, a secret — is a **host-capability callback** the plugin requests back
over the protocol, and the host executes it inside the guarded envelope. Plugin code must never
reach for `reqwest`, `std::net`, `std::process::Command`, `std::fs`, or a vendor SDK that owns its
own socket.

Two consequences follow, and they are what makes the manifest meaningful:

- **The process starts with a cleared environment.** flux spawns every plugin through its single
  guarded process path, which clears the environment and re-adds only a minimal non-secret
  allow-list (`PATH`, `HOME`, `LANG`, `TERM`, `TZ`, `RUST_LOG`, …). A token in flux's own
  environment is simply not present in the plugin's, so `std::env` is not a route to it.
- **Capabilities are deny-by-default.** A fresh host grants nothing; each callback is checked
  against what this plugin's manifest declared. Ask for nothing and you get nothing.

That contract is enforced on the **host** side. The plugin binary itself is trusted, pinned native
code and is not OS-sandboxed by default — a malicious binary could issue direct syscalls rather than
honoring the protocol. Vet a plugin like any other native dependency, and keep every privileged
effect on a host callback so the gates below actually apply. Opt-in
[OS process sandboxing](../security/os-sandbox.md) closes that remaining gap underneath the
capability model.

## The manifest

The manifest *is* the security surface. It is built once with `PluginBuilder` and handed to the host
on the first protocol frame. Here is the shape of the `websearch` plugin's declaration, trimmed to one
operation so the security blocks stay legible — the real plugin declares a second op
(`websearch.provider.list`) and projects `websearch.search` under the public name `web.search` with
`exposed_as`:

```rust
use host_kit::*;

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("websearch", env!("CARGO_PKG_VERSION"))
        .capabilities(Caps {
            http: true,
            http_hosts: vec!["api.tavily.com".into(), "api.duckduckgo.com".into()],
            secrets: vec!["TAVILY_API_KEY".into()],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "tavily_api_key".into(),
            env: vec!["TAVILY_API_KEY".into()],
            description: "Tavily API key (optional; falls back to DuckDuckGo)".into(),
            ..Default::default()
        })
        .datasource(Declaration {
            name: "websearch.results".into(),
            entity: "web.result".into(),
            description: Some("Web search results.".into()),
            capabilities: vec!["search".into(), "get".into()],
            entity_schema: None,
        })
        .operation_typed::<SearchInput, SearchOutput>(
            read_op_typed::<SearchInput>(
                "websearch.search",
                "Search the web (Tavily if configured, else DuckDuckGo). Returns ranked results.",
            ),
            search,
        )
}

fn main() -> Result<(), String> {
    manifest_builder().try_serve()
}
```

Everything this plugin may reach is in that block. `http_hosts` is the entire outbound allow-list;
`secrets` is the entire set of environment keys the host will resolve on its behalf; an operation
that is not registered does not exist.

What the host reads on the wire is the same thing as JSON:

```json
{
  "name": "websearch",
  "version": "0.1.2",
  "capabilities": {
    "http": true,
    "http_hosts": ["api.tavily.com", "api.duckduckgo.com"],
    "secrets": ["TAVILY_API_KEY"]
  },
  "auth": [
    { "purpose": "tavily_api_key", "scheme": "bearer", "env": ["TAVILY_API_KEY"] }
  ],
  "operations": [
    {
      "name": "websearch.search",
      "description": "Search the web …",
      "input_schema": { "type": "object", "properties": { "query": { "type": "string" } } },
      "effects": ["read"],
      "idempotency": "idempotent"
    }
  ]
}
```

`flux plugin status <name>` prints the resolved form of this — op, auth, endpoint and datasource
counts, the capability flags, and whether each declared auth purpose and endpoint currently resolves
(never the value).

## Lifecycle

### Install → configure → call

1. **Install.** The binary is registered as a descriptor under `~/.flux/plugins/`. There are three
   sources with three trust models, and `flux plugin ls` labels which one you have: the
   minisign-signed pack (`verified`), a local `--dir` scan of binaries you built yourself
   (`unverified (local)`), and a `--git` source build (`from-source (unverified)`). Only the signed
   pack carries a per-archive checksum, and only it is re-checked at every spawn. See
   [Plugin trust & signing](../security/plugin-trust.md).
2. **Configure.** A plugin reads no config file at runtime. Configuration means "set the environment
   the *host* resolves on the plugin's behalf" — the declared secret env keys and endpoint URLs from
   the manifest, or a token stored once with `flux auth set <plugin> <purpose>`. If an endpoint
   resolves to a private or loopback address, the operator must additionally grant that plugin under
   `[private_net.plugins]` in `.flux/config.toml`; the grant is intersected with the manifest's own
   `private_hosts`.
3. **Call.** `flux plugin call <name> <op> '<json>'` for debugging and scripting, or let the agent
   path (`flux run`, the REPL, `flux app run`) discover installed plugins and project their
   operations as tools.

### Inside a session

The plugin process is launched **once per session** and its manifest is fetched **once**, which pins
the host's grants for the whole session — a plugin cannot renegotiate its capabilities mid-flight.
After that the exchange is NDJSON frames on the same stdio, in both directions:

| Direction | Frame | Meaning |
|---|---|---|
| host → plugin | `manifest` | Advertise the manifest. Sent once at startup. |
| host → plugin | `operation.call` | Invoke one operation with its validated input. |
| plugin → host | `secret`, `config`, `http.do`, `process.run`, `conn.dial`, `blob.put`, `contribute`, … | A capability callback. The host checks the manifest, performs it, and answers. |
| plugin → host | response | The operation's result, or an error string. |

Every frame carries the `flux.plugin.v1` protocol marker; a mismatch is reported as a version
disagreement naming which side is out of date, not as an opaque decode failure.

Operations without an explicit manifest group land in an on-demand `plugin.<name>` group on the
open-ended CLI path: naming the integration in a request surfaces its catalog for that engine
session, so unrelated installed plugins stay out of every model-stage catalog. After changing a
plugin's surface, regenerate the catalog skill with `flux plugin skill --install`.

## The IO and security contract

Everything below is enforced by the host. The reasoning behind each gate lives in
[Plugin capability sandbox](../security/plugin-sandbox.md); this is the authoring-side summary of
what you must declare to get it.

| Capability | Manifest gate | What the host does for you |
|---|---|---|
| `secret` | `secrets` (env-key allow-list) | Resolve a secret **by purpose**, never by a key chosen at runtime. |
| `config` | `config` (declared names) | Resolve a declared **non-secret** value; a secret-classified key is refused. |
| `http.do` | `http` + `http_hosts` (+ SSRF guard) | Method, headers and body, with auth injected host-side per the declared scheme. |
| `process.run` / `spawn` / `read` / `status` / `kill` | `process` (argv-**prefix** allow-list) | Run or hold a subprocess with captured, capped output; `status` polls a held one for liveness. |
| `conn.dial` / `read` / `write` / `close` | `conn` (`tcp:host:port`, `unix:/path`; `*` matches one segment and Unix `.`/`..` paths are denied) | A raw byte stream for non-HTTP wire protocols. |
| `conn.authenticate` | `conn` + a declared auth purpose or endpoint credential ref | The **host** speaks the in-band handshake and hands back a post-auth connection. |
| `credential` | `credential: true` | Materialize a credential *reference* into its value — the audited exception. |
| `endpoint.discover` | `discover: true` | Ask the host which endpoints exist for a product. |
| `fs.read` | `fs` (path-scoped globs) | Read a host file outside the workspace jail; `..` rejected, size-capped. |
| `blob.put` / `get` / `info` | `blob: true` | A content-addressed scratch store, so file transfers aren't inlined as base64. |
| `contribute` | a declared datasource | Add records to the knowledge index from list operations. |

Four rules matter more than the rest.

**Address endpoints by reference, never by URL.** There is deliberately no capability that hands a
resolved base URL back to a plugin. Declare an `EndpointSpec` and use the `*_ref` IO helpers; the
host resolves the URL from declared env (or a template, or a default), runs it through the SSRF
guard and allow-list, and makes the request. The composed URL never crosses back.

**Never hand-roll auth.** Declare an `AuthScheme` (`Bearer`, `Basic`, `Header { name }`,
`Query { name }`) and pass the *purpose*:

```rust
host.get_json_ref("gitlab.endpoint", path, Some("api_token"))?;
host.send_json_ref("gitlab.endpoint", "POST", path, Some("api_token"), &body)?;
```

The host resolves the purpose and injects the secret per the scheme. The raw value is never returned
to the plugin on this path. Do not build an `Authorization` header, do not base64 in-plugin, do not
read the token from env — you cannot, and you should not want to.

**Process grants are argv prefixes.** Each `process` entry is a whitespace-separated token sequence
matched exactly against the leading argv tokens of every call. `"kubectl"` grants the program with
any arguments; `"kubectl get"` pins the leading subcommand, so a read-shaped grant is *structurally*
unable to `kubectl delete` even when the ambient kubeconfig would allow it. Narrow each operation
further with the `with_process` combinator — the per-op narrowing is intersected with the manifest
grant (it can never widen) and becomes the operation's disclosed authority in approval prompts and
audit records:

```rust
.operation_flexible(
    with_process(
        read_op_typed::<InventoryListInput>("kubernetes.pod.list", "List Kubernetes pods."),
        &["kubectl get"],
    ),
    pod_list,
)
```

**Declare effects and risk honestly.** Every operation starts from `read_op_typed` (`[Read]`,
idempotent) or `write_op_typed` (`[Write, Network]`); a destructive one sets an explicit `Risk` and
`Idempotency`. Effects drive scheduling and approval, so an inaccurate declaration is a safety bug,
not a documentation one. An operation that declares no effects at all is forced through approval as
a conservative `[Process, Network]` — never ship one by accident.

## An operation, end to end

Derive the input and output types, register the typed operation, and let host-kit generate both
schemas from the executable contract:

```rust
use host_kit::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize, Serialize, JsonSchema)]
struct AlertsInput {
    /// Label matchers, e.g. `severity=critical`.
    matchers: Option<Vec<String>>,
    /// Maximum alerts to return.
    limit: Option<i64>,
}

#[derive(Serialize, JsonSchema)]
struct AlertsOutput {
    alerts: Vec<Value>,
    count: u64,
}

fn alerts(input: AlertsInput, host: &mut Host) -> Result<AlertsOutput, String> {
    let _ = input;
    // IO goes through the host: addressed by endpoint reference, authenticated by purpose.
    let body = host.get_json_ref("alertmanager.endpoint", "/api/v2/alerts", Some("basic"))?;
    let alerts = body.as_array().cloned().unwrap_or_default();
    Ok(AlertsOutput { count: alerts.len() as u64, alerts })
}
```

Register it on the builder with
`.operation_typed::<AlertsInput, AlertsOutput>(read_op_typed::<AlertsInput>("alertmanager.alerts", "…"), alerts)`,
then test it hermetically against `MockHost` — no network, no subprocess:

```rust
#[test]
fn alerts_lists_active_alerts() {
    let plugin = manifest_builder().build();
    let mut host = MockHost::default()
        .with_http("/api/v2/alerts", r#"[{"labels":{"alertname":"HighLatency"}}]"#);
    let out = plugin.call("alertmanager.alerts", json!({}), &mut host).unwrap();
    assert_eq!(out["count"], 1);
}
```

`MockHost` matches canned responses by **substring** in insertion order, so give each one a
distinguishing fragment. Assert both the returned value and `host.contributed` when the operation
feeds a datasource.

Two more habits worth forming. Put constraints the derived schema can express *in* the schema
(`required`, enums as Rust enums, `deny_unknown_fields`) so the shared preflight enforces them
identically in `flux plugin call --dry-run` and at runtime. For the constraints a schema cannot
express — conditional targets, aliases, empty-update guards — attach a `.preflight("<op>", rule)`
that calls the same helper the handler uses, so the two verdicts cannot diverge.

## Build, gate, and ship

The plugin workspace is nested and excluded from the root gate, so build and test package-scoped:

```bash
cd plugins
cargo build  -p flux-plugin-<name>
cargo test   -p flux-plugin-<name>
cargo clippy -p flux-plugin-<name> --all-targets -- -D warnings
cargo fmt    -p flux-plugin-<name>
```

Guest binaries depend on the protocol SDK only —
`flux-plugin = { default-features = false, features = ["guest"] }`, which `host-kit` already
selects. The host transport, credential stack, and signed-pack installer are deliberately absent
from that surface; do not enable them from a guest to reach IO. Request IO through a declared
capability instead.

A merged plugin ships with the next **pack release** (`plugins-v*`), which builds every workspace
member on all target platforms, minisign-signs the index, and publishes one checksummed archive per
plugin per target. A new workspace member is picked up automatically. Users then install it with
`flux plugin install <name>`.

## Related docs

- [Using plugins](./using-plugins.md) — install, pin, inspect, and call plugins.
- [Plugin capability sandbox](../security/plugin-sandbox.md) — the reasoning behind each manifest gate.
- [Plugin trust & signing](../security/plugin-trust.md) — supply-chain checks for installed binaries.
- [OS process sandboxing](../security/os-sandbox.md) — opt-in confinement of the raw plugin process.
- [Credentials & secrets](../security/credentials.md) — how a purpose resolves to a value host-side.
- [Kubernetes plugin](./kubernetes.md) — the reference subprocess-capability plugin.
- [SQL plugin](./sql.md) — the reference raw-connection plugin, with host-terminated auth.
