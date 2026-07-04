# Design: integration plugins (consolidated)

**Status: shipped.** This is the consolidated design record for the integration-plugin epic — it
merges the four design docs that together told the story (stories D-08, D-10, D-12, D-14–D-17). The
shipped behavior is documented in the living [architecture](../architecture.md#extensibility) and on
the [story board](../stories/README.md). The follow-on slice (signed release channel + verified
remote install) has since shipped too (v0.2.14) — see [`plugin-distribution.md`](plugin-distribution.md).

Consolidated docs, in narrative order:
1. **Integration plugin pack** (D-08) — the in-repo `plugins/` workspace and the first native plugins.
2. **Process-plugin protocol redesign** (D-10) — the clean unified `flux.plugin.v1` wire protocol.
3. **Plugin protocol parity extensions** (D-12) — additive host capabilities (auth / conn / blob).
4. **fluxplane-plugins parity** (D-12–D-17) — deepening the pack to fluxplane op-parity.

---

# Design: integration plugin pack (in-repo `plugins/`)

**Status (at merge; since shipped):** proposed (story [D-08](../stories/D-08-integration-plugin-pack.md)) · **Layer:** consumes the
redesigned `flux-plugin` (L4, [D-10](integration-plugins.md)) + `flux-datasource`/`flux-secret` (L0) ·
**Home:** an **in-repo nested `plugins/` cargo workspace** (excluded from the root workspace) ·
**Owner:** Timo

## Why

flux ships a production-ready plugin **host** (`flux-plugin`) but **no integration plugins**. A
Slack-channel assistant's "DevOps assistant" scope needs the surface the fluxplane Go bot had: Slack ops, web search,
GitLab, Jira, Confluence, Kubernetes, Loki, Prometheus. Mechanism (decided): **native flux plugins**, not
MCP — each a subprocess speaking flux's protocol. **Prerequisite:** that protocol is redesigned first
([D-10](integration-plugins.md)) so a plugin can contribute datasource records, authenticate by
purpose, and resolve endpoints.

## Home — an in-repo `plugins/` workspace (decision reversed this session)

The original plan put these in a **new sibling `flux-plugins` repo**. **Decided this session: they live
inside the flux repo**, as a nested cargo workspace **excluded from the root workspace** — so the heavy
integration deps (k8s client, cloud SDKs) stay out of the main `flux` gate (`cargo build --workspace`),
yet everything is one repo and one CI. Plugin binaries are subprocesses, so there is **no layering edge**
for `flux-codegate` (which scans only `crates/*`).

```
plugins/                     # its own cargo workspace; not a root member (root: exclude = ["plugins"])
  Cargo.toml                 # path-dep ../crates/{flux-plugin, flux-datasource, flux-secret}
  host-kit/                  # shared helper over D-10's binding SDK: manifest builder, secret-by-purpose
                             # fetch via host callback, dispatch boilerplate, a Record emitter for D-07
  slack/  websearch/  gitlab/  jira/  confluence/  kubernetes/  loki/  prometheus/
                             # one binary crate per integration
```

Each plugin crate is a small `main` that declares a **manifest** (ops, datasources, auth-by-purpose,
endpoints, required host capabilities) and serves the protocol loop. `host-kit` removes the boilerplate so
a new plugin is mostly "declare ops/datasources + implement each against the vendor API."

## Manifest & capability model (over the D-10 protocol)
- A plugin declares its ops (name + input JSON Schema + access/effects/risk/idempotency for the
  authorization floor), its **datasources** (`flux-datasource` `Declaration`s — entities it can
  list/search/contribute), its **auth methods by purpose** + **endpoints**, and the host capabilities it
  needs. The host **denies by default** and authorizes from the manifest — see
  [process-plugin-protocol.md](integration-plugins.md).
- **Secrets** are fetched by purpose via the host protocol (the plugin never reads state files); the host
  resolves a purpose → `flux-secret` material — `env/GITLAB_PERSONAL_TOKEN`, `plugin/slack/main/bot_token`,
  etc. — and can inject it into a host HTTP call (e.g. bearer).
- Ops become policy-gated tools in the consuming agent; the assistant's **D-09 op-grant** list names the
  ops it allows (e.g. `gitlab.*`, `slack.post`), so they run under the headless approver without `--yes`.

## Datasource records (feed D-07) — via the L5 bridge
Where an integration exposes searchable entities (GitLab MRs/issues, Slack channels/users), the plugin
emits **`flux-datasource` `Record`s** via `host-kit`. They reach the D-07 persistent index through a new
**`DatasourceHostCaps`** in `flux-capabilities` (L5): it wraps `flux-plugin`'s `SystemHostCaps` and
services the datasource record/search/get host commands against the index. The bridge lives at L5 (not in
flux-plugin, L4) because the index is L5 — flux-plugin defines only the trait + protocol. The record
contract is identical across local docs and live integrations (the shared `flux-datasource` schema).

## Install + invoke + CI (C-02)
Once built, register the pack with **`flux plugin install [dir]`** (one descriptor per `flux-plugin-*`
binary; `add`/`ls`/`pin`/`rollback` exist too). Invoke one op directly — without an agent — with
**`flux plugin call <name> <op> [json]`** (spawns the binary, drives the op through `DatasourceHostCaps`);
this powers debugging and the live `scripts/smoke-plugins.sh`. The nested `plugins/` workspace builds in a
dedicated **`plugins` CI job** (it's excluded from the root workspace). See
[C-02](../stories/C-02-integration-stack-hardening.md).

## Slices (ship per integration)
1. **Slack ops** (post/edit/react/search/users/channels/thread) + **websearch** (Tavily + DuckDuckGo
   aggregation) — unblocks the assistant MVP (the bot can *answer*, not just receive).
2. **GitLab** (projects, MRs, issues, users, groups, CI/CD) + datasource records.
3. **Jira + Confluence** (issues/projects; pages/spaces).
4. **Kubernetes** (namespaced inventory, allow-listed namespaces).
5. **Loki + Prometheus** (log queries; PromQL + alerts/targets).

## Testing
- Per plugin: a hermetic **op round-trip** through the `flux-plugin` runtime with a stub vendor client
  (no network) — request → manifest op → typed response; assert **capability-deny-by-default** (an op that
  needs an ungranted capability is refused).
- The **bridge**: a plugin emits a record → it is retrievable via the datasource `search`/`get` ops
  (proves `DatasourceHostCaps` ↔ the D-07 index round-trip).
- `host-kit`: a unit test that a declared manifest serializes to the protocol shape the host expects.
- The `plugins/` workspace builds/tests on its own (`cargo build/test` inside `plugins/`), kept out of the
  main flux gate.

## Prior art (copy the op/datasource shapes, not the code)
`fluxplane-plugins/{slack,gitlab,jira,confluence,kubernetes,loki,prometheus,websearch}` — the op sets and
datasource entities are a proven inventory; reimplement against flux's redesigned protocol + `host-kit`.

## Non-goals (v1)
- An OpenAPI **dynamic-tool** plugin (the bot indexes OpenAPI specs as RAG docs via D-07 instead).
- A plugin marketplace / `.dex`-style endpoint+grant+index registry (config + env per integration for v1).
- In-process plugins (everything is a subprocess; that is the flux model).


---

# Design: process-plugin protocol redesign

**Status (at merge; since shipped):** proposed (story [D-10](../stories/D-10-process-plugin-protocol.md)) · **Layer:** L4
(`flux-plugin`) · **Owner:** Timo

## Why

flux's plugin runtime (`flux.plugin.v1`) is host-complete but predates the needs of the integration pack
([D-08](integration-plugins.md)): a plugin **cannot contribute datasource records** ([D-07](datasource-rag.md)),
there is **no auth-by-purpose** (secrets are raw env keys), and **no endpoint** concept for authenticated
APIs. fluxplane solved all three — but its wire protocol grew organically and carries legacy. Before eight
plugins are written against flux's protocol, we redesign it **once, cleanly**, taking fluxplane's capability
surface but not its accreted shape. (The user's steer: *"the protocol for the Go version has evolved over
time — maybe there are parts we can solve more elegantly by changing the process-plugin protocol in a smart
way."*)

## What fluxplane accreted (the cruft to drop)

Learned from `~/projects/fluxplane/fluxplane-plugin`:
- **Dual protocol modes** — a v1 legacy framing *and* a v2 framed mode coexist (`protocol` version string +
  branching). We ship one frame, versioned by the manifest, no mode flag.
- **Three overlapping command families** — `operations.*`, `datasources.*`, and `host.capability.*` are
  bespoke message groups with their own envelopes/lifecycles. We unify them onto **one** request/response
  frame distinguished by direction + a `command` prefix (`op:`/`ds:`/`host:`) — **and drop fluxplane's
  explicit `target` field**, since direction already says who handles a `Request`.
- **Per-call grant negotiation / list round-trips** — `operations.list`, `datasources.list` and ad-hoc
  capability checks happen over the wire at call time. We make the **manifest the single source of truth**,
  fetched once and introspected by the host (capabilities authorized from it, no per-call negotiation).
- **Redundant scoping knobs** — endpoint-ref resolution, fallback modes, and instance plumbing are spread
  across many fields. We keep the *concepts we need* (endpoints, datasource fallback) but as plain manifest
  data, and defer the `.dex`-style registry entirely.

## Target shape (refined in the implementation's design step, plan Phase 2)

### One frame (evolve the existing transport — don't rewrite it)
flux's `PluginHost::call_with_host` (`crates/flux-plugin/src/lib.rs:504-530`) **already** writes a single
`Request` and loops, servicing plugin→host `Request` callbacks inline and awaiting the op's `Response` by
`kind`. So the framing mechanics stay; the redesign is a command-vocabulary + manifest cleanup.
```
Frame {
  id: String,                 // correlation id
  kind: Request | Response | Event,
  command: String,            // "op:slack.message.send" | "ds:search" | "host:http" | …
  payload: Value,             // typed per command
  // responses carry exactly one of:
  ok: bool, result: Value, error: Option<Error>,
}
```
**No `target` field** — *direction* already implies it: a host-initiated `Request` is handled by the
plugin; a plugin-initiated `Request` (mid op) is a host-capability callback. A plugin op call, a datasource
`search`/`get`/`lookup`/`records` contribution, and a host capability request are all just a `command` over
this one frame — same correlation/multiplexing for all three.

### One manifest (fetched once, host-introspected)
- **operations**: `{ name, description, input_schema, effects[], risk, idempotency, secret_purposes[] }`
  — **reuse flux-runtime's `Effect`/permission-subject/`Risk`** vocabulary (add only the idempotency hint +
  `secret_purposes`); do **not** port fluxplane's parallel `Access` enum. `effects` → permission subjects
  at tool projection.
- **datasources**: `flux-datasource` `Declaration`s (entity + capabilities + `EntitySchema` + relations +
  fallback) — the records a plugin contributes/serves into D-07's index.
- **auth**: methods **by purpose** (`bot_token`, `api_token`, …) with env aliases + secret/sensitive
  fields; the host resolves a purpose → `flux-secret` material and can inject it (e.g. bearer) into a host
  HTTP call.
- **endpoints**: named, env-resolved base URLs (GitLab/Jira) — declared, resolved by the host.
- **capabilities**: the host-capability classes the plugin may use (`http`, `process`, `env`, `blob`,
  `conn`) — **deny-by-default**; the host authorizes calls against this declaration, no wire negotiation.

### Host capabilities (host-side, deny-by-default)
`http` (with **secret-by-purpose injection**), `process`, `env`, `blob`, `conn`. The trait stays in
`flux-plugin`; the concrete `SystemHostCaps` is extended (process plumbing reused). The **datasource**
host commands (`ds:records`/`ds:search`/`ds:get`) are serviced by an L5 impl
([`DatasourceHostCaps` in flux-capabilities](integration-plugins.md)) because the index is L5 — flux-plugin
defines only the trait + protocol.

### Plugin-binding SDK (Rust)
The analog of fluxplane's `pluginbinding`: typed `operation(spec, handler)` + `datasource(spec, handler)`
registration, manifest builders, a `serve()` loop over the one frame, and a host-call client. `EntitySchema`
derived from a record struct via `flux-datasource-derive`. A new plugin becomes "declare ops/datasources +
implement each against the vendor API."

## Cutover
`flux.plugin.v1` is **removed** (no dual mode). The `echo`/`caps` fixtures and the CLI call sites
(`load_plugin_tools`, `discover` in `crates/flux-cli/src/main.rs`) move to the new protocol. `flux-plugin`
stays **L4**; it gains a dep on the L0 `flux-datasource` crate (D-07) for the datasource/manifest types.

## Testing (hermetic)
- A fixture round-trips an op call, a `ds:records` contribution, and a host `secret`-by-purpose fetch over
  the single frame (no network).
- Capability-deny-by-default: an ungranted `http`/`secret` call is refused.
- Manifest → tool projection: an op's `access`/`effects` produce the expected permission subjects.

## Non-goals (v1)
- A `.dex`-style endpoint+grant+index registry (manifest data + config only).
- In-process plugins (everything is a subprocess — the flux model).
- `context.build` / `evidence.observe` plugin commands (flux already has `flux-evidence`; map on demand).

## Reuse, don't reimplement
- `flux-plugin`'s `PluginHost`/`serve` transport + `SystemHostCaps` process plumbing (extend).
- `flux-secret::Ref` + `SecretResolver` (secret-by-purpose). The `flux-datasource` types (D-07).
- Prior art (shapes only): `fluxplane-plugin/{protocol,manifest,pluginbinding}`.


---

# Design: plugin protocol parity extensions (D-12)

**Status:** shipped (D-12 done) · **Pillar:** Core · **Layer:** L4 (`flux-plugin`) + L1 (`flux-system` dialer) ·
**Owner:** Timo · **Story:** [D-12](../stories/D-12-plugin-protocol-parity.md) ·
**Epic:** [fluxplane-plugins-parity.md](integration-plugins.md)

## Why

The fluxplane plugins flux still needs to port reach their backends three ways flux's host capabilities don't
yet support. All three are **additive** to `flux.plugin.v1` — no fallback flags, no second protocol mode
(clean cutover): existing manifests and the 8 shipped plugins keep working untouched. The host stays the
single IO boundary — a plugin never opens a socket, reads an env var, or builds an auth header itself.

The current surface (`SystemHostCaps::handle`, `crates/flux-plugin/src/lib.rs`): `process.run`, `secret`
(by purpose, env-only), `endpoint`, `http.do` (Bearer-by-purpose + SSRF guard via `flux_system::net::guard_url`).

## Slice A — non-Bearer auth injection

Today `http.do` injects only `bearer_purpose`. jira/confluence build `Authorization: Basic base64(email:token)`
*inside the plugin* (so the plugin sees the raw secret — the thing the host model exists to prevent);
alertmanager/grafana/homer/opsgenie need basic / custom-header / query-param auth. The host should inject all
of these by purpose so the plugin never handles the secret.

**`AuthMethod` gains a scheme + a user field** (both `#[serde(default)]`, so old manifests = `Bearer`):

```rust
pub enum AuthScheme {            // serde: lowercase tag, default Bearer
    #[default] Bearer,           // Authorization: Bearer <secret>            (unchanged behaviour)
    Basic,                       // Authorization: Basic base64(<user>:<secret>)
    Header { name: String },     // <name>: <secret>           (e.g. GenieKey, PRIVATE-TOKEN)
    Query  { name: String },     // ?<name>=<secret>
}

pub struct AuthMethod {
    pub purpose: String,
    pub env: Vec<String>,        // resolves the secret (unchanged)
    pub description: String,
    pub scheme: AuthScheme,       // NEW (default Bearer)
    pub user_env: Vec<String>,    // NEW: for Basic, the username/email half (e.g. JIRA_EMAIL)
}
```

**`http.do` accepts `auth_purpose`** (the existing `bearer_purpose` stays, treated as `auth_purpose` with an
implicit Bearer scheme). The host looks up the method, resolves the secret via the existing `resolve_purpose`,
and injects per `scheme`:
- `Basic` → resolve `user_env` (first set) as the username; `base64(user:secret)`.
- `Header { name }` → set header `name: secret`.
- `Query { name }` → append `name=secret` to the URL query.

The injected secret is **never** returned to the plugin. `user_env` values are config (an email), not secrets,
so they resolve from declared env directly. host-kit grows `Host::http(.., auth_purpose, ..)` plus
`basic_op`/header convenience and an `AuthMethod::basic(purpose, user_env, env)` builder.

## Slice B — raw connection dialer (`conn.*`)

sql (MySQL/PG/SQLite), docker (unix socket), asterisk (AMI over TCP), and a native client-go-style kubernetes
all need a **socket**, not HTTP. flux-system has no dialer — only `guard_url`. Add one, reusing the same egress
policy (host→IP resolution, loopback/private/link-local rejection unless allowed).

**flux-system (L1):** `net::dial(target: DialTarget, allow_private: bool) -> io::Result<DialStream>` where
`DialTarget ∈ { Tcp { host, port }, Unix { path } }`; TCP runs the `guard_url` IP policy before connecting;
optional host-terminated TLS (`tokio-rustls`) for `Tcp` when requested. `DialStream` is an
`AsyncRead + AsyncWrite` handle.

**flux-plugin host caps:** four commands on `SystemHostCaps`, backed by a `Mutex<HashMap<u64, DialStream>>`
connection registry keyed by an opaque `conn_id`:
- `conn.dial { kind: "tcp"|"unix", host?, port?, path?, tls? }` → `{ conn_id }`
- `conn.read  { conn_id, max }` → `{ data_b64, eof }`
- `conn.write { conn_id, data_b64 }` → `{ written }`
- `conn.close { conn_id }` → `{ ok }`

**Capability gate:** a new `PluginCapabilities.conn: Vec<String>` allow-list of permitted targets
(`"tcp:host:port"`, `"unix:/path"`; glob on host/port allowed). A fresh `SystemHostCaps` grants none; a dial to
an undeclared target is denied before the guard even runs. The registry is per-`SystemHostCaps` (per call
scope), so connections don't leak across plugin invocations.

host-kit: `Host::conn_dial(target) -> Conn`, where `Conn` implements `Read`/`Write` by round-tripping
`conn.read`/`conn.write` — so a Rust DB/AMI/Docker client library can be handed a `Conn` as its transport.

## Slice C — blob store (`blob.*`)

File-upload ops (slack files, jira/confluence attachments) shouldn't inline base64 through the op input
(argv/JSON size + log noise). A `blob_ref` indirection mirrors fluxplane's `blob put`:
- `blob.put  { name, data_b64 }` → `{ blob_ref }`
- `blob.get  { blob_ref }` → `{ data_b64 }`
- `blob.info { blob_ref }` → `{ name, size, sha256 }`

Backed by a guarded per-plugin blob dir under the flux state dir (`~/.flux/blobs/<plugin>/`), gated by a new
`PluginCapabilities.blob: bool`. `blob_ref` is an opaque content-addressed handle. host-kit: `Host::blob_put`
/`blob_get`/`blob_info`; CLI `flux plugin blob put <name> <file>` is a later convenience, not part of D-12.

## Layering / safety
- The dialer lives in **flux-system (L1)**; `flux-plugin (L4)` only adds the `conn.*`/`blob.*` command
  handlers and registries — no new cross-layer dep, `flux-codegate` stays green.
- Every new capability is **deny-by-default** and gated by an explicit `PluginCapabilities` grant built from
  the plugin's manifest, exactly like `process`/`secrets`/`http` today.
- The auth secret and blob bytes never cross back to the plugin except where the plugin explicitly asked for
  them (`blob.get`); auth injection is host-only.

## Testing (hermetic)
- **A:** `MockHost` records the injected header; `SystemHostCaps` unit test asserts Basic = `base64(user:tok)`,
  Header/Query placement, and that `bearer_purpose` still works. A jira-style call needs no base64 in-plugin.
- **B:** a loopback `tokio` echo server: dial → write → read round-trips through the registry; a dial to a
  private/undeclared target is rejected by the grant and the guard.
- **C:** put → info (size/sha256) → get round-trips; an unknown `blob_ref` errors; blob denied without the grant.

## Rollout
Slice A is the committed deliverable (small, unblocks D-15 + D-14's base64 cleanup). B and C follow; whatever
is unfinished in a session is logged in the D-12 story Progress. No CHANGELOG entry until a slice lands on the
gate.


---

# Design: fluxplane-plugins parity (the integration-plugin epic)

**Status:** D-12 / D-13 / **D-14–D-17 shipped** (all portable native plugins are implemented) ·
**Pillar:** Agent · **Layer:** L4 (`flux-plugin`) + the `plugins/` workspace ·
**Owner:** Timo · **Stories:** [D-12](../stories/D-12-plugin-protocol-parity.md) ·
[D-13](../stories/D-13-plugin-skill-command.md) · [D-14](../stories/D-14-deepen-native-plugins.md) ·
[D-15](../stories/D-15-observability-ai-plugins.md) · [D-16](../stories/D-16-datastore-infra-plugins.md) ·
[D-17](../stories/D-17-telephony-plugins.md)

## Why

flux shipped **8** native plugins under [D-08](../stories/D-08-integration-plugin-pack.md) over the
[D-10](../stories/D-10-process-plugin-protocol.md) protocol. The source they were modelled on —
`~/projects/fluxplane/fluxplane-plugins` — ships **26 marketplace plugins**, and flux's 8 cover only a
*fraction* of their operations (gitlab 6/60+, slack 5/30, jira 3/~20, k8s 5/24). The goal of this epic is
**full native parity**: every *portable* fluxplane plugin rewritten as a native flux plugin at full op
coverage, plus a generated **plugin skill** so the catalog is self-documenting to the agent.

"Native" matters: flux deliberately does **not** wrap fluxplane's Go binaries or speak MCP — each plugin is a
Rust subprocess on flux's own `host-kit` over the `flux.plugin.v1` protocol, capability-gated and inside the
same safety envelope as the agent's own tools.

## Parity matrix (the 26 marketplace plugins → flux disposition)

| Disposition | fluxplane plugins | Story |
|---|---|---|
| **Native, shallow** — port-deepen to full op set ✅ **done (D-14)** | confluence, gitlab, jira, kubernetes, loki, prometheus, slack, websearch | **D-14** |
| **Native — HTTP pack** ✅ **done (D-15)** | alertmanager, grafana, opsgenie, huggingface | **D-15** |
| **Native — datastore/infra pack** ✅ **done (D-16)** | sql, docker, aws | **D-16** |
| **Native — telephony pack** ✅ **done (D-17)** | asterisk, homer | **D-17** |
| **Covered differently — NOT ported** | clock→`now`, system→`sys_info`, sleep→builtin, git→tool group, openai/ollama→providers, duckduckgo/tavily→folded into flux `websearch` | — |
| **Deliberate divergence — NOT ported** | vision/websearch *aggregators*, openapi *generator* | — |

### Why some plugins are not ported (so "parity" is well-defined)
- **clock / system / sleep / git** — flux already exposes these as **builtin ops / a tool group**
  (`now`, `sys_info`, `sleep`, the `git` group in `flux-tools`), not as plugins. A plugin would be redundant.
- **openai / ollama** — these are **providers** in flux (the model layer), not integration plugins. Their
  fluxplane "ops" (image/vision/model.list, generate/chat/embed) are provider surface, addressed there.
- **duckduckgo / tavily** — flux's `websearch` plugin already **folds both backends in** (Tavily primary, DDG
  fallback); fluxplane split them into provider plugins behind an aggregator. flux's single plugin is simpler.
- **vision / websearch aggregators** + **openapi generator** — these rely on fluxplane's provider-call and
  spec-driven-generation surfaces that flux intentionally omits (see below). flux RAG-indexes OpenAPI specs
  via D-07 instead of generating a tool-per-endpoint plugin (an explicit D-08 non-goal).

## Protocol gap → D-12

flux's `SystemHostCaps` (`crates/flux-plugin/src/lib.rs`) services `process.run`, `secret` (by purpose, env),
`endpoint`, and `http.do` (Bearer-by-purpose injection + SSRF guard). The missing plugins need three
**additive** host capabilities, designed in [plugin-protocol-parity.md](integration-plugins.md):

1. **Non-Bearer auth injection** — Basic/header/query by purpose. jira/confluence hand-roll
   `Authorization: Basic base64(email:token)` *inside the plugin* today; alertmanager/grafana/homer/opsgenie
   need basic / `config` / `GenieKey`. Unblocks D-15 and lets D-14 delete the base64 hand-rolling.
2. **Raw connection dialer** (`conn.*`) — a guarded tcp/unix socket dialer (flux-system has none today, only
   `guard_url`). sql/docker/asterisk reach backends over a socket, not HTTP. Gates D-16/D-17.
3. **Blob store** (`blob.*`) — a `blob_ref` indirection so file-upload ops don't inline base64. Gates the
   file-upload ops in D-14/D-15.

**Deliberately omitted** from flux's protocol (the D-10 "drop fluxplane's cruft" decision): provider/
capability-call (the aggregator mechanism), context providers, evidence observers, and dual protocol modes.
Parity is **operational** (the integrations an agent can drive), not a byte-for-byte protocol clone.

## Skill generation → D-13

fluxplane's `fluxplane-plugin skill` command *generates* a Claude-format `SKILL.md` + `references/<plugin>.md`
from installed-plugin manifests (that is exactly what produced `~/.claude/skills/fluxplane-plugin/`). flux
gets the analogue **`flux plugin skill`**, designed in [plugin-skill-generation.md](../archive/designs/plugin-skill-generation.md):
it renders the discovered flux-plugin manifests into a Claude-format `flux-plugin` skill so the agent
knows which `flux plugin call` ops exist, their inputs, and their auth — without hard-coding a catalog.

## Sequencing

```
D-12 (protocol: auth → conn → blob)         D-13 (flux plugin skill)   ← this session: D-13 + D-12 start
        │                                            │
        ├── D-14 (deepen the 8, drop base64) ────────┤  (skill refresh after each)
        ├── D-15 (alertmanager, grafana, opsgenie, huggingface)   [needs auth]
        ├── D-16 (sql, docker, aws)                              [needs conn + blob]
        └── D-17 (asterisk, homer)                               [needs conn]
```

D-13 is independent (no protocol dependency) and shipped first. D-12's auth/conn/blob slices unlocked the
native ports. **D-14 through D-17 have shipped**: the original 8 plugins were deepened to op-parity, then the
9 remaining portable plugins were ported natively. Each plugin slice was developed package-scoped, then the
full `plugins/` workspace gate was run together.

### Host protocol extensions landed with D-14
The fidelity pass added two capabilities to `flux.plugin.v1` (the way D-12 added auth/conn/blob):
- **Managed background processes** — `process.spawn`/`read`/`status`/`kill`, a per-session registry in
  `SystemHostCaps` (beside `conns`/`blobs`), backed by `flux_system::System::spawn_background`
  (`ManagedChild`: piped+capped stdout/stderr, `kill_on_drop`, argv-only, env cleared+allow-listed). Gated by
  the manifest `process` allow-list. This is what lets the host hold a long-lived `kubectl port-forward` — the
  plugin process being one-shot is irrelevant since **the host runs and holds all IO**, and one host instance
  is shared across a plugin's op calls (`load_plugin_tools`). (Replaced the earlier, mistaken "plugin can't
  hold a process" punt.)
- **Binary HTTP body** — `http.do` accepts `body_b64` and, with `response_binary: true`, returns the raw
  bytes as `body_b64` (16 MiB cap); host-kit `Host::http_bytes`. Byte-exact upload **and** download (the
  earlier `String`-body lossiness is gone).

## Authoring pattern

Each plugin is a `plugins/<name>/` crate in the nested workspace (excluded from the root gate), binary
`flux-plugin-<name>`, built on `host-kit`'s `PluginBuilder` + `read_op`/`write_op` + `MockHost` (reference:
`plugins/gitlab/src/main.rs` for HTTP, `plugins/kubernetes/src/main.rs` for CLI). Op shapes are copied (not
code) from `~/projects/fluxplane/fluxplane-plugins/<plugin>/manifest.go`. Each slice: full gate green in
`plugins/`, a smoke entry in `scripts/smoke-plugins.sh`, and `flux plugin skill refresh` to regenerate the
catalog.

## Accepted residuals

- **Docker streaming/hijack ops** (`exec`, `stats`, log follow, image build/push, event stream) are not faked:
  the shipped Docker plugin covers the core Engine REST lifecycle over the guarded Unix `conn.*` stream, while
  those operations need a later long-lived stream/hijack design.
- **SQL** supports PostgreSQL over a hand-rolled protocol client with SCRAM/MD5/cleartext auth and MockHost
  frame tests. MySQL is a clear unsupported error; SQLite is unsupported by design because plugins have no host
  file capability. A live Postgres smoke remains the release confidence step.

## Non-goals
- Wrapping fluxplane's Go binaries, or any MCP bridge — plugins are native Rust.
- The omitted protocol surfaces (provider-call, context, evidence, dual modes).
- A plugin marketplace / `.dex`-style endpoint registry.
- Porting the builtin/provider-covered plugins (clock/system/sleep/git/openai/ollama/duckduckgo/tavily).
