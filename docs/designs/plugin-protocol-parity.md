# Design: plugin protocol parity extensions (D-12)

**Status:** in progress · **Pillar:** Core · **Layer:** L4 (`flux-plugin`) + L1 (`flux-system` dialer) ·
**Owner:** Timo · **Story:** [D-12](../stories/D-12-plugin-protocol-parity.md) ·
**Epic:** [fluxplane-plugins-parity.md](fluxplane-plugins-parity.md)

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
