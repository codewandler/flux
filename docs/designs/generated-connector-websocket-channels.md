# Generated connector WebSocket channels

**Status:** accepted program design · **Epic:** [C-481](../stories/C-481-generated-connector-websocket-channels-epic.md)
· **First proof:** Asterisk ARI 22.10.1

## Decision

`flux-connectors` is authoritative for Asterisk ARI and for declarative RFC 6455 connector-channel
handshakes. Flux owns guarded execution and the transport-neutral binding driver. Exchange owns
durable tenant channel instances, authorization and agent fan-out. A declaration never opens a
socket, a runtime never invents a vendor event name, and an Exchange caller never chooses a tenant,
credential, endpoint or placement.

Asterisk ARI and AMI were both removed from the plugin pack in Flux v0.52.0 by D-249 before this
program began. This design does not resurrect either surface. `flux-connectors` remains authoritative
for ARI; a future AMI adapter would be separately designed as a stateful, protocol-rich native
integration rather than folded into this declarative channel contract.

Slack Socket Mode remains D-220's vendor-specific transport. Its URL-ticket/envelope-acknowledgement
protocol is not made generic merely because RFC 6455 is underneath it. This design covers ordinary
declarative WebSocket handshakes such as ARI: derive a relative path from a declared service base,
place declared config and auth, optionally negotiate subprotocols, then receive JSON events.

## Verified baseline

On 2026-08-02, after rebasing the isolated worktree to published tag `v0.52.1` at
`24c2ff21`:

```text
$ cargo search codewandler-flux-core --limit 1
codewandler-flux-core = "0.52.1" ...
```

That published family retains D-241's guarded endpoint-reference WebSocket capability for plugins,
but D-249 already removed the Asterisk plugin. It does not contain a generic connector-channel
WebSocket runtime. Both the workspace manifest and crates.io report `0.52.1` at this baseline.

The existing records disagree with the new authority decision and are superseded where they do:

- `docs/designs/asterisk-ari.md` records the superseded v0.51.1 plugin implementation. D-249's
  removal remains authoritative; ARI now gains a generated channel without reviving that plugin.
- D-215/D-220 treated `transport = "socket"` as synonymous with Slack's vendor protocol. A generic
  RFC 6455 connect declaration is now a peer transport; Slack Socket Mode remains its own adapter.
- D-241 remains valid safety evidence. Its reusable session implementation moves inward to
  `flux-system`; any remaining generic plugin capability continues to impose its own
  manifest/ref/session policy.

## Ownership and dependency direction

```text
flux-connectors declaration + catalogue + zero-I/O plan
                         |
                         v
flux-system guarded native/remote WebSocket session
                         |
                         v
flux-channels transport-neutral connector binding driver
                         |
                         v
flux-exchange durable supervisor + grants + subscriber fan-out
```

The direction is deliberate:

- `flux-connectors` remains offline and ships facts plus a plan. It holds no client, resolver,
  selected system or task.
- `flux-system` knows bytes, frames, addresses, TLS and cancellation, not connectors or event names.
- `flux-channels` knows binding semantics and delivery, not tenant persistence or HTTP APIs.
- Exchange knows tenants, connections, grants, durable records and subscribers. It does not build a
  second vendor request path.

## Connector declaration

### Socket connect specification

`ChannelBinding` gains an optional `SocketConnectSpec`, legal and required only when
`transport = "socket"` describes the generic RFC 6455 transport. It contains:

- `path`: a relative WebSocket path resolved against the binding service's `base_url`;
- ordered query parameter declarations, each fixed or bound from channel-scoped configuration;
- fixed headers whose values are non-secret declaration literals;
- existing connector authentication requirements, expressed with the same `AuthRequirement`
  vocabulary operations use;
- optional WebSocket subprotocol strings.

The path is relative by construction. A whole authority in the binding would let provider bytes
select an origin independently of the service contract and is refused at load. `ws`/`wss` is derived
from the validated service `http`/`https` URL at planning time; a connector cannot smuggle a second
scheme or userinfo through `path`.

Socket declarations fail closed:

- a connect block on webhook/poll is invalid;
- a generic socket binding without one is invalid, except a named vendor-specific transport such as
  Slack Socket Mode selected by the consuming runtime;
- fragments, userinfo, absolute URLs, traversal and brace placeholders without matching config are
  invalid;
- fixed headers cannot name hop-by-hop or handshake-owned fields (`Host`, `Connection`, `Upgrade`,
  `Sec-WebSocket-Key`, `Sec-WebSocket-Version`, `Sec-WebSocket-Protocol`, `Authorization`);
- subprotocol tokens are validated and duplicates refused;
- every auth credential belongs to the connector/service declaration and signing-only inbound
  credentials can never authenticate an outbound socket.

### Configuration is binding-scoped

Configuration adds `channel.<binding>.query.<parameter>`. The stored field remains addressed by the
ordinary `(tenant, provider, service, kind, name)` port, but the declaration says the destination is
one channel binding rather than every operation in a service.

That scope is the safety property. Reusing `query.<parameter>` would pin the same value onto unrelated
REST operations; `app` for ARI events must not appear on every ARI call. A socket plan reads only
config fields whose target names its binding. Operation request composition ignores them.

Optional channel config is represented honestly. `subscribe_all` defaults to boolean `false`; its
absence and explicit false produce `subscribeAll=false`, while a stored true produces
`subscribeAll=true`. Defaults are declared and typed, not invented by the host.

### Event identity and payload shape

`EventDecl::name` remains the local stable member identity and trigger label. It gains
`wire_value: Option<String>` for an exact vendor discriminator spelling. Absence means the local name
is also the wire value. A present value may differ only in spelling; it does not create an open
runtime label.

`ChannelBinding::payload_root` says that the complete decoded JSON event is the delivered typed
payload. It is mutually exclusive with field projection entries in `payload`: a declaration either
delivers the root or constructs a projected object, never both and never an implicit fallback.

Runtime discrimination is a closed lookup:

```text
wire discriminator -> one declared EventDecl.wire_value/name -> local EventDecl.name
```

Unknown or malformed discriminators are dropped and counted. They never become labels, because a
vendor-controlled label would let a new upstream event wake a trigger no reviewed connector declared.

### Manifest and catalogue are complete host inputs

The generated manifest finishes the deferred authentication/configuration surface. It carries:

- authority, runtime, service/base URL and allowed host template;
- every credential declaration and placement, without a value;
- every auth alternative used by operations or socket bindings;
- every configuration field: service, label, help, example, format, required/default/choices and all
  binding destinations;
- events with local name, wire value, schema/default/group;
- channels with socket connect, discriminator, payload-root/projection, verification, reply,
  subscription and setup facts.

`connector-catalog` publishes the same facts as dependency-free `&'static` data. A host composes a
socket without reading provider TOML or parsing generated Flux. Field-census tests derive the source
IR field set and fail when either projection drops one.

### Zero-I/O channel plan

`connector_pack::channel_plan` accepts catalogue connector/binding facts, a tenant-bound
`Configuration` port and tenant-bound `Credentials` port. It returns a `PreparedChannelPlan`:

- exact `ws`/`wss` URL with encoded path and ordered query;
- fixed plus auth headers;
- requested subprotocols;
- expected host/authority and binding/event facts needed by the runtime;
- redacted debug/display output and structured credential/config provenance that names addresses,
  never values.

It does no DNS, owns no client, opens no socket and constructs no Flux runtime. Placement is checked
by the consumer before calling it so a shared deployment with no admissible substrate fails before
credentials are read. The pack still validates the final URL against the declared authority after
substitution, so a stored value cannot reshape the origin.

## Generated Asterisk ARI binding

The Asterisk provider gains one channel:

```text
name             ari-events
transport        socket
path             /events
auth              Basic (existing ARI username + password)
query app         required channel-scoped connection setting
query subscribeAll optional boolean, default false
discriminator     body.type
payload            complete JSON root
```

Each subtype of the official ARI `Event` model becomes one declared event. The local name is
lowercase kebab (`channel-created`), the wire value is exact PascalCase (`ChannelCreated`), and the
schema is the fully resolved subtype including inherited event fields. A runtime delivery label is
`<channel-instance>.channel-created` and its payload is the original typed JSON object.

Two exact censuses are required:

1. every upstream source operation is either one ordinary REST operation or the single
   `ari-events` socket binding, and nothing appears only in generated output;
2. every upstream `Event` subtype is exactly one event declaration and every declaration maps back
   to one subtype.

`GET /events` must no longer be discarded as an empty/silent operation. It is accounted for by the
channel, never also emitted as an HTTP operation.

## Guarded WebSocket execution

### The system port

The reusable portion of D-241 moves into `flux-system` behind
`GuardedNetwork::open_websocket_scoped`. The call receives a prepared endpoint, scoped private-net
admission, handshake metadata, limits and cancellation and returns opaque
`GuardedWebSocketSession` ownership. The only public operations are bounded `read` and `close`.

Native execution performs one guarded DNS resolution, pins the selected address into the TCP dial,
uses the declared hostname for TLS verification and SNI, and performs the HTTP upgrade without
logging URL query or auth headers. It handles ping/pong and close internally. No caller receives the
underlying stream or a reusable credential-bearing request.

Defaults:

- 1 MiB maximum frame and assembled message;
- 32 queued application messages;
- five-second maximum graceful close;
- cancellation aborts connect/read/reconnect and closes ownership;
- binary/text/close/timeout/protocol failures remain distinct.

### Remote equivalence

The remote-system protocol adds open/read/close operations with the same opaque session ownership and
limits. The selected `ExecutionSystem` performs DNS, private-network policy, TLS and frames. A local
coordinator never opens a fallback socket if the remote is absent or fails.

Authentication remains opaque across the execution boundary according to the selected system's
trust contract. A shared deployment may send a prepared credential only to an operator-trusted remote
system selected before credential resolution; it may not send one to a caller-named endpoint.

## Transport-neutral connector channels

`ConnectorChannel` becomes a binding driver with transport implementations for webhook and generic
WebSocket. Slack Socket Mode may feed raw JSON into it, but retains its vendor-specific connect and
ack loop. Shared code owns:

- the binding's closed event set;
- discriminator extraction and wire-value lookup;
- `when` narrowing;
- root or field payload projection;
- delivery labels and delivery-id metadata;
- declared reply dispatch through the existing executor seam;
- malformed/undeclared counters.

`ChannelContext` replaces the loose start parameters and carries the `Deliverer`, cancellation token
and selected `ExecutionSystem`. Existing inbound listeners bind where they already bind and ignore
the selected system. Outbound WebSocket channels must use it.

### Reconnect classification

Reconnect starts at one second and caps at thirty seconds with jitter. Tests inject the clock and
jitter source. A stable connection resets the backoff.

Reconnectable:

- DNS/connect/TLS/network loss after an admissible plan;
- peer close not classified as a declaration/protocol failure;
- HTTP 5xx handshake response.

Terminal until configuration/catalogue change:

- invalid declaration, missing/invalid config or missing auth;
- no admissible placement;
- HTTP 400, 401, 403 or 404 handshake;
- binary ARI frame or other declared protocol violation.

Malformed JSON and undeclared event types are per-message drops with counters, not reconnect storms.

## Placement

This program consumes C-478's typed execution placement and adds no parallel enum.

- local/single-tenant profiles may dial on the native system, including a private endpoint only when
  its exact scope is operator-admitted;
- shared profiles may use only an operator-provisioned endpoint reference or a trusted selected
  remote system;
- tenant- or agent-supplied whole-authority hosts are always refused;
- missing admissible placement is decided before `channel_plan` reads credentials.

The endpoint/placement resolver is a host port. Flux supplies the execution vocabulary; Exchange's
built-in server may bind it from protected deployment configuration; hosted products may implement
the port without changing request bodies.

## Exchange lifecycle

### Durable channel record and supervisor

A channel belongs to the authenticated tenant and references an existing connection plus a declared
binding. The durable record contains an opaque channel id, connector, connection instance, binding,
selected declared events and lifecycle metadata. It contains no endpoint, credential or resolved
placement.

`ChannelStore` is persistent and required for mutation. A supervisor exists independently of
subscribers, resolves placement, prepares the plan, opens the vendor connection, reconnects transient
failures and restores records after process restart. Terminal configuration failures remain visible
and stopped until an operator changes the connection/catalogue or restarts it through mutation.

Credential or connection-setting rotation restarts every affected supervisor so a live socket cannot
continue with stale authority. The supervisor closes the old session before resolving new values.

### Operator API

Only signed-in operators may mutate channels:

- `GET /api/channels`;
- `POST /api/channels` with connector, existing connection, binding and selected declared events;
- `PUT /api/channels/{id}` changing selected events;
- `DELETE /api/channels/{id}` stopping and deleting it.

Tenant is derived from the principal. Endpoint, credential, tenant and placement fields are absent
from request types, not accepted and ignored. Connector/binding/events are catalogue keys and every
event must belong to that binding.

### Inbound grants

Grants add explicit inbound entries:

```text
{ connector, binding, events }
```

Existing serialized grants default to an empty inbound list, which means no inbound access. A grant
cannot name an event the binding does not declare. Explicit deny keeps precedence. Operation grants
do not imply channel access and channel grants do not expose credentials.

### Agent subscription

`GET /api/subscribe` is one authenticated WebSocket. Commands subscribe/unsubscribe by opaque channel
id and carry a client request id. Responses include correlated acknowledgements or structured
refusals. Cross-tenant channel ids are indistinguishable from unknown ids to the agent.

Event envelopes carry connector, binding, declared local event name, receive time and raw typed
payload. They never carry endpoint, credential, auth header or resolved placement.

Delivery is live at-most-once. There are no persisted subscriber cursors, acknowledgements or replay
records. Disconnects, Exchange restart and periods with no subscribers may lose events.

One vendor connection fans out to all admitted subscribers. Each subscriber gets a bounded 32-event
queue. Overflow records the drop and closes only that subscriber; it never blocks the vendor read
loop or another subscriber.

## Observability and disclosure

Logs may name tenant-safe opaque channel id, connector, binding, local event name, state transition,
counter and classified error. They must not contain endpoint URLs, query values, credentials, auth
headers or private event payloads.

Anonymous catalogue/onboarding surfaces publish declarations and capability status only. They do not
publish tenant channels, endpoint refs, deployment placement or payloads. Exchange marks `subscribe`
live in its descriptor, console, README and public capability page only after the route and end-to-end
tests exist.

## Verification matrix

Flux:

- DNS rebinding, private address, undeclared endpoint, missing auth, TLS/SNI, cancellation,
  queue/frame bounds and native/remote equivalence;
- deterministic reconnect and terminal handshake classification;
- mock ARI `ChannelCreated` -> `<channel>.channel-created` with complete payload;
- placement profiles proving shared caller-host refusal.

`flux-connectors`:

- invalid socket declarations fail at load;
- manifest/catalogue/pack field completeness;
- exact URL/query/Basic header composition with secret-free debug;
- two-way Asterisk route and event subtype censuses;
- scoped generation first, whole-catalogue generation only at integration.

Exchange:

- tenant derivation, operator-only mutation, default-deny inbound grants, event subsets and
  cross-tenant opaque-id refusal;
- one vendor socket fan-out, no replay, slow-subscriber isolation, restoration and rotation restart;
- direct local, operator endpoint, selected remote and refused shared-host profiles;
- no endpoint, credential, auth header or payload in logs/anonymous catalogue.

An environment-gated live Asterisk smoke connects an application, observes representative lifecycle
events and proves cancellation closes the socket.

## Release and compatibility

Publish in dependency order:

1. Flux guarded WebSocket/runtime version;
2. `flux-connectors` catalogue/pack version requiring that Flux line;
3. Exchange with Flux and connector dependency families bumped in one commit.

There is no plugin-pack compatibility release in this sequence: v0.52.0 already removed both
Asterisk surfaces. Stored-recording binary/blob parity remains a separate connector capability gap,
not a gate for undoing a removal that has already shipped.

## Out of scope

- durable event replay, subscriber acknowledgements and cursors;
- persistent subscriber sessions;
- generalizing Slack Socket Mode into a declarative handshake;
- reintroducing the removed ARI or AMI plugin surfaces;
- inventing a second placement vocabulary;
- allowing callers to supply whole authorities, endpoints, credentials, tenants or placement.
