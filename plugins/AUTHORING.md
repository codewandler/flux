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

## The one rule that governs everything: the host does all privileged IO

**A plugin performs no privileged IO of its own.** Every side effect — HTTP, a socket, a subprocess, a
file, a secret read — is a **host-capability callback** the plugin requests back over the protocol, and
the host (`SystemHostCaps`) executes it inside the guarded envelope. A plugin must never reach for
`reqwest`, `std::net`, `std::process::Command`, `std::fs`, or a vendor SDK that owns its own socket.

The host side is enforced; the plugin binary itself is trusted, pinned code rather than an OS
sandbox. Review installed plugins like dependencies, and keep all privileged effects on host
callbacks so the manifest gates below actually apply.

- **The plugin process is launched with a cleared environment.** flux spawns every plugin through its
  single guarded process path (`flux_system::System::spawn_interactive` → `build_command`), which
  `env_clear()`s and re-adds only a minimal non-secret allow-list (`PATH`, `HOME`, `LANG`, `TERM`, `TZ`,
  `RUST_LOG`, …). So a plugin **cannot read the host's secrets from `std::env`** — `FOO_API_TOKEN` set
  in flux's environment is simply not present in the plugin's. The only way to a secret is the gated
  `secret` capability below. (Regression test: `crates/flux-plugin/tests/host.rs`
  → `plugin_cannot_read_host_env`.)
- **Host capabilities are deny-by-default and manifest-scoped.** A fresh host grants nothing; each
  callback is checked against what the plugin's manifest declared (`SystemHostCaps::with_manifest`).
  Private/loopback network access additionally requires an operator grant in `.flux/config.toml`.
  Ask for nothing and you get nothing.

## Lifecycle: install → configure → call

1. **Install** — register the binary as a descriptor at `~/.flux/plugins/<name>.toml`. There are
   **three install sources**, each with its own trust model; the descriptor's verification label
   (shown by `flux plugin ls`/`status`) tells them apart:

   | Source | Command | Trust model | `ls` label |
   | --- | --- | --- | --- |
   | **signed pack** (D-46..49) | `flux plugin install <name>[@<version>]` / `--all` | minisign-signed index + per-archive sha256 verified before unpack into `~/.flux/plugins/bin/<name>/<version>/`; the sha256 is re-checked at every spawn (`spawn_verified`) — drift is a hard refusal | `verified` (or `hash drift`) |
   | **local scan** (`--dir`) | `flux plugin install --dir [path]` (default `plugins/target/release`) / `flux plugin add <name> <program>` | none — registers already-built binaries by path; trusted because *you* built them | `unverified (local)` |
   | **from source** (`--git`, D-87) | `flux plugin install --git <url> [--tag <t> \| --rev <r> \| --branch <b>] [--bin <name>] [--force]` | clone → `cargo build --release --locked` → register; **building unverified source is code execution**, gated behind an explicit confirm that discloses the resolved commit (non-interactively `FLUX_ALLOW_SOURCE_BUILD=1`). Provenance = the git URL + resolved commit, **not** a signed-pack hash | `from-source (unverified)` |

   ```
   # signed pack (no source tree, no toolchain needed):
   flux plugin install gitlab slack

   # local scan while authoring:
   (cd plugins && cargo build --release)      # → plugins/target/release/flux-plugin-<name>
   flux plugin install --dir                  # register every built binary (local, unverified)
   flux plugin add <name> /abs/path/to/flux-plugin-<name>   # …or one at a time

   # from a git URL (e.g. a private GitLab-hosted plugin the pack channel can't serve):
   flux plugin install --git https://gitlab.example/group/flux-plugin-foo.git --tag v1.0.0
   ```
   `--git` clones into a cache (`~/.flux/plugins/src/<repo>/`), pins the resolved commit, and copies
   the built binary into `~/.flux/plugins/bin/<name>/git-<commit>/` so the descriptor's program path
   is stable; re-installing the same resolved commit is an idempotent no-op, `--force` rebuilds. A
   repo that is not a flux plugin (no `[[bin]] flux-plugin-*` target) fails with a clear error, not a
   raw cargo dump. **Cross-cutting caveat:** the cloned plugin's own build-time dependencies must
   resolve on the installing machine — a private SDK dep must be crates.io-public, reachable as a git
   dep, or served from a private Cargo registry (GitLab has no native one). All three sources go
   through the single guarded process path: clone + build run via `flux_system::System` (argv-only,
   no shell), and the plugin spawns env-cleared.

   The descriptor records the program path + args; that exact binary is what flux launches. A plugin
   binary is **trusted, pinned code** — vet it like a dependency before you install it.
2. **Configure (auth setup)** — a plugin reads no config at runtime. Instead, set the **environment
   variables the host resolves on the plugin's behalf**: the secret env keys and endpoint URLs the
   plugin **declared** in its manifest (e.g. `GITLAB_PERSONAL_TOKEN`, `GITLAB_URL`). The host reads
   these from **its own** environment at call time, gated by the manifest. Configuration is "set the
   declared env before you call", not an interactive step inside the plugin. If an endpoint resolves
   to a private/loopback host, also grant that plugin under `[private_net.plugins]` in
   `.flux/config.toml`.
3. **Call** — `flux plugin call <name> <op> '<json>'` (debugging/scripting), or let the agent path
   (`flux run` / `flux app run`) discover installed plugins and project their ops as tools. The plugin
   is launched once per session; its manifest is fetched once and pins the host's grants for that
   session. On the open-ended CLI agent path, visible operations without an explicit manifest group
   are placed in an on-demand `plugin.<name>` group: naming the plugin in the user's request surfaces
   its catalog for that engine session. This keeps unrelated installed integrations out of every
   model-stage catalog. Declare an explicit group when you need different surfacing behavior; a force-on
   group (`surface_when = []`) remains always visible.

After changing a plugin's surface, regenerate the catalog skill: `flux plugin skill --install`.

## Host capabilities (the full set)

All requested via the `host-kit` `Host`/`GuestHost`; serviced by `SystemHostCaps`
(`crates/flux-plugin/src/lib.rs`). Each is gated by the manifest's `capabilities` (`PluginCapabilities`).

| Capability | Manifest gate | What it does |
|---|---|---|
| `secret` | `secrets` (env-key allow-list) | Resolve a secret **by purpose** (manifest auth method) → value. |
| `config` | `config` (declared names) | Resolve a declared **non-secret** config value (e.g. jira's `cloud_id`); a secret-classified env key is refused. |
| `credential` | `credential: true` | Materialize a credential **reference** into its raw value for in-band-auth raw-socket protocols (e.g. Postgres SCRAM); redactor-registered, never returned via any discovery/endpoint path. |
| `http.do` | `http: true` + `http_hosts` + SSRF guard | HTTP method/headers/body; **auth injected by the host** per `AuthScheme`; binary via `body_b64` (request) / `response_binary` → `body_b64` (response, 16 MiB cap). Private hosts require `private_hosts` plus config. |
| `process.run` | `process` (argv-**prefix** allow-list, C-90) | Run a subprocess to completion; captured, capped output. |
| `process.spawn`/`read`/`status`/`kill` | `process` | Start/drain/poll/stop a long-lived host-managed child (e.g. `kubectl port-forward`). |
| `conn.dial`/`read`/`write`/`close` | `conn` (`tcp:host:port` / `unix:/path` allow-list, SSRF-guarded) | A raw TCP/Unix byte stream for non-HTTP protocols (SQL wire, Docker socket, AMI). |
| `conn.authenticate` | `conn` (an already-dialed `conn_id`) + a declared auth method or endpoint credential ref | **Host-terminated** in-band auth (D-31): the host speaks the startup + SCRAM/MD5 handshake itself and hands back a post-auth connection — the plugin never receives the password. |
| `endpoint.discover` | `discover: true` | Cross-plugin endpoint discovery (D-26): ask the host what endpoints exist for a product. |
| `fs.read` | `fs` (`FsReadScope` path allow-list) | Read a **host** file outside the workspace jail matching a declared scope; `..` traversal rejected, size-capped; `secret: true` scopes are redactor-registered. |
| `blob.put`/`get`/`info` | `blob: true` | A scratch blob store (SHA-256 ref) so file up/downloads aren't inlined as base64. |
| `contribute` | (datasource declared) | Add `flux-datasource` `Record`s to the D-07 index from list ops. |

**Process grants are argv prefixes (C-90).** Each `process` entry is a whitespace-separated token
sequence matched exactly against the leading argv tokens of every `process.run`/`process.spawn`
call: `"kubectl"` grants the program with any arguments, `"kubectl get"` pins the leading
subcommand so a read-shaped grant is structurally unable to `kubectl delete`. Declare **exactly the
verbs your handlers issue** (subcommand-first argv; trailing flags are unconstrained), and narrow
each operation to its own verbs with the `with_process(op, &["kubectl get"])` combinator — the
narrowing is enforced at callback time on top of the manifest gate (intersection; it can never
widen) and becomes the op's disclosed `process.exec` authority in approval prompts and audit. A
per-op entry outside the manifest grant is rejected at load time. See the C-90 decision record in
`docs/designs/integration-plugins.md`; `kubernetes` and `aws` are the reference declarations.

There is deliberately **no `endpoint` URL-handback capability** (retired, D-32): endpoints are
declared in the manifest (`PluginBuilder::endpoint(EndpointSpec)`) and addressed **by reference** —
the host resolves the base URL (declared env keys, a default, or a host-composed template) and puts
it on the wire via the `*_ref` IO paths (`http_ref`/`get_json_ref`/`send_json_ref`/`conn_dial_ref`);
the resolved URL is never returned to the plugin.

### Secret resolution & auth injection (never hand-roll auth)

A plugin declares an auth method — a `purpose`, the env keys that satisfy it, and an `AuthScheme`
(`Bearer` / `Basic` / `Header { name }` / `Query { name }`; for `Basic` the username half resolves
from the method's `user_env` keys). To authenticate a request you pass the **purpose**, not a token:

```rust
host.get_json_ref("gitlab.endpoint", path, Some("api_token"))?;   // GET, host injects the declared scheme
host.send_json_ref("gitlab.endpoint", "POST", path, Some("api_token"), &body)?;
```

The host resolves the purpose to a value from the granted env keys and injects it per the scheme
(`Authorization: Bearer …`, `Basic base64(user:token)`, a custom header, or a query param). **The raw
secret is never returned to the plugin on this path.** Never build an `Authorization` header yourself,
never base64 in-plugin, never read the token from env — declare the scheme and let the host inject.
(A dynamic-token flow — e.g. log in to get a JWT — is fine: fetch the credential via `host.secret`,
`http.do` the login, then send the returned token; you still never touch raw env.)

## The rules (checklist)

1. **Declare everything in the manifest** — every op, every `secrets`/`process`/`conn`/`http`/`blob`/
   `config`/`credential`/`fs`/`discover` capability, every `http_hosts`/`private_hosts` entry, every
   auth method and endpoint, every datasource. Undeclared → denied at runtime with a clear error.
2. **Never do IO directly** — no `reqwest`, `std::net`, `std::fs`, `std::process::Command`. Use the
   `Host` callbacks. (Vendor SDKs that insist on owning a `TcpStream` don't fit; sit a minimal client
   on `conn_*` instead.)
3. **Never read env directly** — you can't (it's cleared), and you shouldn't. Ask the host via the
   declared `secret`/`config` names; endpoints resolve host-side — address them by reference.
4. **Never hand-roll auth** — declare an `AuthScheme` and pass the purpose; the host injects.
5. **Pick real effects and authority** — every closed op starts from `read_op_typed` (`[Read]`,
   idempotent) or `write_op_typed` (`[Write, Network]`); the raw `read_op`/`write_op` constructors
   remain for the explicit flexible adapter. A write/destructive op sets `Risk`. Effects disclose
   scheduling and risk, while the manifest's declared `http`/`process`/`conn`/`secret`/datasource
   capabilities provide the typed resource requirements enforced by policy. Stable semantic effects
   (`write_db`, `send_external`, `delete`, `money`) add their own action requirement.
   Missing or inconsistent access declarations and unknown semantic effects reject the operation
   before it is advertised. An empty-effects op is forced through approval as a conservative
   `[Process, Network]` — never ship one by accident.
6. **Contribute knowledge** — for list ops, `host.contribute(&records)` so results feed the search
   index (optional but expected where natural).
7. **Test hermetically** — one `MockHost` test per op (below). No network/subprocess in unit tests.
8. **Declare constraints in the schema; preflight the rest (D-88)** — every op gets a shared
   preflight that host-kit runs in BOTH the CLI's `--dry-run` (via the auto-registered internal
   `plugin.validate` op) and runtime dispatch, so the two verdicts can never disagree. What the
   derived schema declares is enforced locally: `required` (blank strings count as missing, matching
   flex extraction), `enum` (use a Rust enum field), `range(min = 1)` on ids, `length(min = 1)` on
   arrays, typed element structs, and `additionalProperties: false` (via `deny_unknown_fields`) to
   hard-reject unknown fields — on open schemas unknown fields only *warn*. For constraints a schema
   cannot express (conditional targets, aliases, regex validity, empty-update guards), attach
   `.preflight("<op>", rule)` on the builder and have the rule call the SAME helper the handler
   uses.

## Authoring recipe (one op, end-to-end)

Edit `plugins/<name>/src/main.rs` (reference: `plugins/gitlab/src/main.rs` for HTTP,
`plugins/kubernetes/src/main.rs` for process). For each op:

1. **Declare** closed contracts in `manifest_builder()` with
   `.operation_typed::<Input, Output>(read_op_typed::<Input>("<name>", "<desc>"), <handler>)`
   (or `write_op_typed`). Derive `Deserialize + Serialize + JsonSchema` on `Input` and
   `Serialize + JsonSchema` on `Output`; host-kit generates both manifest schemas from those types.
   Add any new `Caps.secrets`, `EndpointSpec`, `AuthMethod`, or `.datasource(...)` in the same
   builder. For an intentionally open result, use the explicit `operation_flexible` adapter and
   attach `with_output_schema(...)` only when a truthful stable envelope can still be described.
   The legacy value-only `.operation(...)` spelling is deprecated.
2. **Handler**: accept the typed `Input` and return the typed `Output`. Host-kit reports path-aware
   decode errors, normalizes serde aliases/defaults before shared preflight, and passes the
   already-decoded input to the handler. Keep `operation_flexible` only for a deliberately open
   vendor payload, with the reason documented beside it. Do IO through
   `host.get_json_ref`/`send_json_ref`/`http_ref`/`http_bytes_ref`
   (endpoint-reference HTTP) or `run`/`conn_*`/`blob_*`, and for knowledge ops emit `Record`s via
   `host.contribute`. The per-plugin rollout is tracked in [TYPED-MIGRATION.md](TYPED-MIGRATION.md).
3. **Test** against a `MockHost` (`with_http`/`with_process`/`with_http_bytes` match by **substring** in
   insertion order — give each canned response a distinguishing substring): assert the returned value
   **and** `host.contributed`.
4. **Serve fallibly** — make the binary entry point `fn main() -> Result<(), String>` and return
   `manifest_builder().try_serve()`. The legacy `build`/`serve` wrappers panic on invalid assembly;
   `try_build`/`try_serve` preserve the duplicate or manifest/handler mismatch as a startup error.

Operation names must be unique within a plugin manifest. `PluginBuilder` rejects identical and
conflicting duplicates before serving; the host also rejects collisions between built-ins, custom
tools, and multiple installed plugins with both sources in the error. Do not rely on registration
order as an override mechanism.

`host-kit` (`plugins/host-kit/src/lib.rs`) is the SDK: `PluginBuilder`,
`operation_typed`/`operation_flexible`, `read_op_typed`/`write_op_typed`, the typed
`Host` (`secret`/`config`/`http`/`get_json`/`send_json`/`http_bytes`/`http_ref`/`http_bytes_ref`/
`get_json_ref`/`send_json_ref`/`run`/`process_*`/`conn_*`/`conn_dial_ref`/`conn_authenticate`/
`credential`/`credential_for_endpoint`/`blob_*`/`contribute`), and `MockHost`. Contract types
(`AuthMethod`/`AuthScheme`/`OperationSpec`/`PluginCapabilities`) live in `crates/flux-plugin/src/lib.rs`,
re-exported through host-kit.

`flux-plugin` is feature-partitioned. Guest binaries must use only the protocol SDK:

```toml
flux-plugin = { version = "0.24", default-features = false, features = ["guest"] }
```

The `guest` surface contains frames, manifest/operation contracts, `GuestHost`, `PluginHandler`, and
`serve`; it deliberately excludes the host transport (`reqwest`, credentials, runtime/system), JS
hooks, and signed-pack installer/archive stack. `host-kit` already selects this configuration, so
normal first-party plugins inherit the lightweight boundary automatically. The flux host uses the
default `host + hooks + pack` feature set. Do not enable those host features from a guest to reach
IO—request IO through a declared host capability instead. At the C-69 cutover the host-kit normal
dependency tree fell from the review's roughly 237 entries to 80; a structural test keeps excluded
host packages out and caps accidental tree growth. A clean release build of the representative
`flux-plugin-alertmanager` binary (empty target directories, same machine and warm source cache)
fell from 41.106 s / 2,014,936 bytes before the cutover to 15.098 s / 1,608,624 bytes after it, so
the guest binary is about 20.2% smaller rather than regressing.

## Gate

Build/test **package-scoped** from the nested workspace (it's excluded from the root gate):

```
cd plugins
cargo build  -p flux-plugin-<name>
cargo test   -p flux-plugin-<name>
cargo clippy -p flux-plugin-<name> --all-targets -- -D warnings
cargo fmt    -p flux-plugin-<name>
cargo test   -p codewandler-flux-host-kit --test guest_dependency_boundary
```

A new plugin is a new member in `plugins/Cargo.toml`. Heavy vendor deps live here, never in the root
flux gate. Add a representative op to `scripts/smoke-plugins.sh` (env-gated) before release.

## Releasing: where your binary ends up

A merged plugin ships with the next **pack release** — the plugin release channel, separate from the
core `v*` releases. `.github/workflows/release-plugins.yml` (dispatched manually with a `version`
input; never triggered by a tag) builds the whole `plugins/` workspace on five native runners,
packages one archive per plugin per target (`flux-plugin-<name>-<version>-<target>.tar.xz`, `.zip`
on Windows), generates + minisign-signs `plugins-index.json`, and publishes everything as the
**`plugins-v<version>`** GitHub release. A new workspace member is picked up automatically — no
workflow edit needed. Users then get it via the plugin CLI: `flux plugin install <name>`.

**Never hand-push a `plugins-v*` tag.** The workflow creates the tag itself at release time; a
hand-pushed one triggers the core cargo-dist plan job against a tag it can't build and red-Xs it.
