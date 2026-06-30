# Authoring a flux plugin

The canonical guide to writing a native flux integration plugin. If you are adding or deepening a
plugin (the `plugins/` pack, stories D-08/D-14/D-15/D-16/D-17), read this first — it is the contract.
For the pack overview and install/invoke commands see [README.md](README.md); for the protocol/host
implementation see `crates/flux-plugin/src/lib.rs`.

## What a plugin is

A plugin is a **subprocess binary** that speaks flux's framed `flux.plugin.v1` NDJSON protocol over
stdin/stdout. It declares a **manifest** (a name, a set of operations, and the capabilities/auth/
endpoints it needs) and implements each operation. flux projects every operation as a policy-gated
tool, so a plugin op traverses the **same safety envelope** as the agent's built-in tools —
authorization → approval → guarded IO. There are no bypass paths; do not add one.

"Native" means: a Rust subprocess on flux's own `host-kit` over `flux.plugin.v1`. Not an MCP bridge,
not a wrapper around a vendor binary.

## The one rule that governs everything: the host does all IO

**A plugin performs no privileged IO of its own.** Every side effect — HTTP, a socket, a subprocess, a
file, a secret read — is a **host-capability callback** the plugin requests back over the protocol, and
the host (`SystemHostCaps`) executes it inside the guarded envelope. A plugin must never reach for
`reqwest`, `std::net`, `std::process::Command`, `std::fs`, or a vendor SDK that owns its own socket.

This is enforced, not merely advised:

- **The plugin process is launched with a cleared environment.** flux spawns every plugin through its
  single guarded process path (`flux_system::System::spawn_interactive` → `build_command`), which
  `env_clear()`s and re-adds only a minimal non-secret allow-list (`PATH`, `HOME`, `LANG`, `TERM`, `TZ`,
  `RUST_LOG`, …). So a plugin **cannot read the host's secrets from `std::env`** — `FOO_API_TOKEN` set
  in flux's environment is simply not present in the plugin's. The only way to a secret is the gated
  `secret` capability below. (Regression test: `crates/flux-plugin/tests/host.rs`
  → `plugin_cannot_read_host_env`.)
- **Host capabilities are deny-by-default and manifest-scoped.** A fresh host grants nothing; each
  callback is checked against what the plugin's manifest declared (`SystemHostCaps::with_manifest`).
  Ask for nothing and you get nothing.

## Lifecycle: install → configure → call

1. **Install** — register the binary as a descriptor at `~/.flux/plugins/<name>.toml`:
   ```
   cd plugins && cargo build --release        # → plugins/target/release/flux-plugin-<name>
   flux plugin install                        # register every built flux-plugin-* binary
   #  …or one at a time:
   flux plugin add <name> /abs/path/to/flux-plugin-<name>
   ```
   The descriptor records the program path + args; that exact binary is what flux launches. A plugin
   binary is **trusted, pinned code** — vet it like a dependency before you install it.
2. **Configure (auth setup)** — a plugin reads no config at runtime. Instead, set the **environment
   variables the host resolves on the plugin's behalf**: the secret env keys and endpoint URLs the
   plugin **declared** in its manifest (e.g. `GITLAB_PERSONAL_TOKEN`, `GITLAB_URL`). The host reads
   these from **its own** environment at call time, gated by the manifest. Configuration is "set the
   declared env before you call", not an interactive step inside the plugin.
3. **Call** — `flux plugin call <name> <op> '<json>'` (debugging/scripting), or let the agent path
   (`flux run` / `flux app run`) discover installed plugins and project their ops as tools. The plugin
   is launched once per session; its manifest is fetched once and pins the host's grants for that
   session.

After changing a plugin's surface, regenerate the catalog skill: `flux plugin skill --install`.

## Host capabilities (the full set)

All requested via the `host-kit` `Host`/`GuestHost`; serviced by `SystemHostCaps`
(`crates/flux-plugin/src/lib.rs`). Each is gated by the manifest's `capabilities` (`PluginCapabilities`).

| Capability | Manifest gate | What it does |
|---|---|---|
| `secret` | `secrets` (env-key allow-list) | Resolve a secret **by purpose** (manifest auth method) → value. |
| `endpoint` | — (config, not a secret) | Resolve a named base URL from declared env keys. |
| `http.do` | `http: true` + SSRF guard | HTTP method/headers/body; **auth injected by the host** per `AuthScheme`; binary via `body_b64` (request) / `response_binary` → `body_b64` (response, 16 MiB cap). |
| `process.run` | `process` (argv[0] allow-list) | Run a subprocess to completion; captured, capped output. |
| `process.spawn`/`read`/`status`/`kill` | `process` | Start/drain/poll/stop a long-lived host-managed child (e.g. `kubectl port-forward`). |
| `conn.dial`/`read`/`write`/`close` | `conn` (`tcp:host:port` / `unix:/path` allow-list, SSRF-guarded) | A raw TCP/Unix byte stream for non-HTTP protocols (SQL wire, Docker socket, AMI). |
| `blob.put`/`get`/`info` | `blob: true` | A scratch blob store (SHA-256 ref) so file up/downloads aren't inlined as base64. |
| `contribute` | (datasource declared) | Add `flux-datasource` `Record`s to the D-07 index from list ops. |

### Secret resolution & auth injection (never hand-roll auth)

A plugin declares an auth method — a `purpose`, the env keys that satisfy it, and an `AuthScheme`
(`Bearer` / `Basic { user_env }` / `Header { name }` / `Query { name }`). To authenticate a request you
pass the **purpose**, not a token:

```rust
host.get_json(url, Some("api_token"))?;            // GET, host injects the declared scheme
host.send_json("POST", url, Some("api_token"), &body)?;
```

The host resolves the purpose to a value from the granted env keys and injects it per the scheme
(`Authorization: Bearer …`, `Basic base64(user:token)`, a custom header, or a query param). **The raw
secret is never returned to the plugin on this path.** Never build an `Authorization` header yourself,
never base64 in-plugin, never read the token from env — declare the scheme and let the host inject.
(A dynamic-token flow — e.g. log in to get a JWT — is fine: fetch the credential via `host.secret`,
`http.do` the login, then send the returned token; you still never touch raw env.)

## The rules (checklist)

1. **Declare everything in the manifest** — every op, every `secrets`/`process`/`conn`/`http`/`blob`
   capability, every auth method and endpoint, every datasource. Undeclared → denied at runtime with a
   clear error.
2. **Never do IO directly** — no `reqwest`, `std::net`, `std::fs`, `std::process::Command`. Use the
   `Host` callbacks. (Vendor SDKs that insist on owning a `TcpStream` don't fit; sit a minimal client
   on `conn_*` instead.)
3. **Never read env directly** — you can't (it's cleared), and you shouldn't. Ask the host via the
   declared `secret`/`endpoint` purposes.
4. **Never hand-roll auth** — declare an `AuthScheme` and pass the purpose; the host injects.
5. **Pick real effects** — every op is `read_op` (`[Read]`, idempotent) or `write_op`
   (`[Write, Network]`); a write/destructive op sets `Risk`. An empty-effects op is forced through
   approval as a conservative `[Process, Network]` — never ship one by accident.
6. **Contribute knowledge** — for list ops, `host.contribute(&records)` so results feed the search
   index (optional but expected where natural).
7. **Test hermetically** — one `MockHost` test per op (below). No network/subprocess in unit tests.

## Authoring recipe (one op, end-to-end)

Edit `plugins/<name>/src/main.rs` (reference: `plugins/gitlab/src/main.rs` for HTTP,
`plugins/kubernetes/src/main.rs` for process). For each op:

1. **Declare** in `manifest_builder()`: `.operation(read_op|write_op("<name>", "<desc>", json!(<schema>)), <handler>)`,
   plus any new `Caps.secrets`, `EndpointSpec`, `AuthMethod`, or `.datasource(...)`.
2. **Handler** `fn <handler>(input: Value, host: &mut Host) -> Result<Value, String>`: validate input,
   do IO through `host.get_json`/`send_json`/`http_bytes`/`run`/`conn_*`/`blob_*`, and for knowledge
   ops emit `Record`s via `host.contribute`.
3. **Test** against a `MockHost` (`with_http`/`with_process`/`with_http_bytes` match by **substring** in
   insertion order — give each canned response a distinguishing substring): assert the returned value
   **and** `host.contributed`.

`host-kit` (`plugins/host-kit/src/lib.rs`) is the SDK: `PluginBuilder`, `read_op`/`write_op`, the typed
`Host` (`secret`/`endpoint`/`http`/`get_json`/`send_json`/`http_bytes`/`run`/`process_*`/`conn_*`/
`blob_*`/`contribute`), and `MockHost`. Contract types (`AuthMethod`/`AuthScheme`/`OperationSpec`/
`PluginCapabilities`) live in `crates/flux-plugin/src/lib.rs`, re-exported through host-kit.

## Gate

Build/test **package-scoped** from the nested workspace (it's excluded from the root gate):

```
cd plugins
cargo build  -p flux-plugin-<name>
cargo test   -p flux-plugin-<name>
cargo clippy -p flux-plugin-<name> --all-targets -- -D warnings
cargo fmt    -p flux-plugin-<name>
```

A new plugin is a new member in `plugins/Cargo.toml`. Heavy vendor deps live here, never in the root
flux gate. Add a representative op to `scripts/smoke-plugins.sh` (env-gated) before release.
