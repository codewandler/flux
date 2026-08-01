---
title: Connector channels
description: "Serve a manifest-defined connector webhook: place the manifest, select a binding, configure the listener, and understand today's verification and reply limits."
---

# Connector channels

A `connector` channel turns one named binding in a connector manifest into an inbound event source.
The manifest supplies vendor semantics — event names, routing selectors, payload fields, verification
posture, and optional reply metadata — while your program supplies deployment details such as the
listen address and secrets.

:::caution Current support
flux currently serves only manifest bindings with `transport = "webhook"` and an explicit
`verification.kind = "none"`. A binding that requires HMAC verification is validated and then
refused at startup because raw-body signature verification is not implemented. Connector replies are
also not automatic yet: accepted requests receive `202`, and flux does not invoke the manifest's
reply operation after the journey finishes.
:::

## Put the manifest in place

Use the `.connector.toml` file emitted by the connector publisher or its generation tooling. flux
does not currently provide a connector installer command, so place the file yourself or point the
channel at it explicitly.

By default, flux resolves manifests under `~/.flux/connectors/`:

| declaration | expected manifest |
|---|---|
| `connector "widget"` | `~/.flux/connectors/widget.connector.toml` |
| `connector "widget"` + `service "hooks"` | `~/.flux/connectors/widget-hooks.connector.toml` |

For example:

```bash
mkdir -p ~/.flux/connectors
cp /path/to/widget-hooks.connector.toml ~/.flux/connectors/widget-hooks.connector.toml
```

Alternatively, set `manifest "/absolute/path/widget-hooks.connector.toml"` on the channel. This is
an operator-controlled override; it does not change the identity declared inside the file.

Before writing the channel declaration, inspect these manifest fields:

```toml
connector = "widget"
service = "hooks"

[[channels]]
name = "hooks"
transport = "webhook"
```

The program's `connector`, optional `service`, and `binding` values must exactly match the manifest's
`connector`, optional `service`, and selected `[[channels]].name`. flux checks those identities again
when it loads the file and refuses a mismatch before opening a listener.

## Declare the listener

This channel serves the `hooks` binding from the example above on loopback:

```flux
channel widget_events
  kind "connector"
  connector "widget"
  service "hooks"
  binding "hooks"
  addr "127.0.0.1:8790"
  path "/widget/events"

trigger on_build_finished
  on "widget_events.build.finished"
  agent assistant
```

The available channel settings are:

| setting | required | meaning |
|---|---:|---|
| `connector` | yes | Connector id declared at the top of the manifest. |
| `binding` | yes | Exact name of the manifest's `[[channels]]` binding. |
| `service` | no | Named connector service; also changes the default filename. |
| `manifest` | no | Explicit manifest path, instead of `~/.flux/connectors/…`. |
| `addr` | yes for webhooks | Socket address on which flux listens. |
| `path` | no | POST path; defaults to `/`. A missing leading slash is added. |
| `token` | no on loopback | Static bearer token. It is required for a non-loopback listener under the currently servable verification posture. |
| `credentials` | when referenced | Map from a credential name in the manifest to this deployment's secret value. |

Keep deployment secrets out of the manifest and program. If a binding names a credential, map its
exact name to a secret reference:

```flux
channel widget_events
  kind "connector"
  connector "widget"
  service "hooks"
  binding "hooks"
  addr "127.0.0.1:8790"
  credentials { "widget.signing_secret": secret "WIDGET_SIGNING_SECRET" }
```

flux resolves the reference at load. Supplying the credential does **not** make an HMAC binding
servable today; the listener still refuses to start until it can verify the signature against the raw
request body.

## How an event becomes a trigger

A connector request must be a valid JSON POST. The binding can read a discriminator from one header
or a dotted path in that parsed body, then map dotted body paths into flux payload symbols. For
example, a manifest may declare:

```toml
events = ["build.finished", "build.failed"]

[channels.verification]
kind = "none"
verified = false

[channels.discriminator]
source = "header"
name = "X-Widget-Event"

[channels.payload]
project = "project"
status = "status"
```

This request:

```bash
curl -i http://127.0.0.1:8790/widget/events \
  -H 'Content-Type: application/json' \
  -H 'X-Widget-Event: build.finished' \
  --data '{"project":"docs","status":"passed"}'
```

fires the exact label `widget_events.build.finished` with this payload:

```json
{
  "project": "docs",
  "status": "passed"
}
```

Mapped fields that are missing or `null` are omitted. If the binding declares a delivery-id selector,
flux also adds its string value as `delivery_id`. Header selectors are supported for the discriminator
and delivery id; payload fields themselves come from the JSON body.

When a binding has no discriminator, it fires under the channel name (`widget_events` in this
example). With a discriminator, a missing or undeclared event is logged, answered with `204 No
Content`, and not delivered. An accepted declared event is scheduled for delivery and answered with
`202 Accepted`; the HTTP response does not wait for or contain journey results.

## Authentication and exposure

`verification.kind` is a claim the manifest must state explicitly, not a feature flux silently
assumes:

- `none` means the vendor supplies no signature. It is the only posture currently served.
- `hmac` is checked for a coherent algorithm, signature source, signed-data template, and mapped
  secret, then refused because flux cannot yet verify the signature over the raw request bytes.
- `connection` is refused for a webhook binding. Socket bindings are not implemented, and poll
  bindings belong in a scheduled flow that calls the connector's cursor operation.

For an address outside loopback, a currently servable `none` binding must also set a non-empty token:

```flux
channel widget_events
  kind "connector"
  connector "widget"
  service "hooks"
  binding "hooks"
  addr "0.0.0.0:8790"
  token secret "WIDGET_WEBHOOK_TOKEN"
```

Callers must then send `Authorization: Bearer <token>`. flux refuses a non-loopback listener without
the token and refuses an empty token even on loopback. The built-in listener is plain HTTP, so expose
it through trusted TLS termination rather than sending a bearer credential over an untrusted network.
If the vendor can authenticate only with a signature that flux cannot yet verify, do not expose the
listener as though the manifest's HMAC claim were enforced.

Every channel-triggered run still goes through the program's authorization, approval, and guarded-IO
envelope. Treat the mapped payload as untrusted input and set a narrow program `permissions` ceiling;
the bearer token controls who can trigger the listener, not what the resulting run may do.

## Reply metadata is not reply execution

A binding may name a reply operation and bind its inputs to payload symbols:

```toml
[channels.reply]
operation = "widget-messages-post"
result = "text"

[channels.reply.bind]
project = "project"
```

flux validates that the operation is published by the manifest, that every bound symbol exists in
the payload map, and that the live app has a tool with that operation name. A missing tool stops the
app before the listener is bound.

That startup check does not dispatch the operation. The connector handler currently acknowledges an
accepted event with `202`, runs the trigger asynchronously, and logs delivery failures; it does not
turn the journey result into a vendor API call. If the workflow must call the vendor today, make that
outbound operation an explicit step in the journey rather than relying on `[channels.reply]`.

## Deployment checklist

- Use the publisher-generated manifest and select its exact connector, service, and binding names.
- Confirm the selected binding is a webhook with explicit verification `none`; do not downgrade an
  HMAC binding by editing the file.
- Prefer a loopback listener. For non-loopback, configure a non-empty secret-backed bearer token and
  trusted TLS termination.
- Send valid JSON and route triggers to the exact channel label or `<channel>.<event>` label declared
  by the binding.
- Expect `202` for accepted events and `204` for missing or undeclared discriminator values, not a
  synchronous journey result.
- Give the program only the permissions its channel-triggered journeys need.

See [Channel inventory and capabilities](./inventory.md) for how connector channels compare with the
generic webhook, Slack, A2A, and other built-in kinds.
