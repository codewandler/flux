---
id: D-215
title: "The generic `connector` channel kind — one arm instead of one adapter per vendor (epic)"
pillar: Agent
status: backlog
epic: connector-channels
note: "EPIC — build_channels is a closed match with one arm per vendor, and the slack arm's last act is to hand-build a chat.postMessage that flux-connectors already compiles. A channel binding is DATA: transport + verification + payload map + a reply operation. Reading it retires 217 lines of vendor Rust and makes the next vendor zero"
---

# The generic `connector` channel kind — one arm instead of one adapter per vendor (epic)

## Goal

Let a flux program declare an ingress surface **by naming a connector and one of its published
channel bindings**, so `flux-channels` gains one generic `connector` arm instead of a hand-written
adapter per vendor.

The inbound counterpart of D-214. Where the Tool pack makes a connector's **operations** callable, this
makes its **events** deliverable — and makes the reply to an event an ordinary operation call through
`Executor::dispatch`, rather than a second, unpoliced request path inside a channel adapter.

## Context — verified against this tree

- `build_channels` dispatches `kind` through a **closed `match`**
  (`crates/flux-channels/src/adapters/mod.rs:46`); an unknown kind is a hard load error (`:63`). A
  plugin cannot supply a channel kind, and neither can a connector.
- One arm, `slack`, is 217 lines of vendor Rust. Its last act builds a `chat.postMessage` by hand from
  `channel`, `text` and `thread_ts` (`crates/flux-channels/src/adapters/slack.rs:150-154`) — the three
  body parameters of an operation flux-connectors already compiles.
- `Channel::start` receives exactly one seam, `Arc<dyn Deliverer>`
  (`crates/flux-channels/src/channel.rs:22`), and `Deliverer` has one method,
  `deliver(label, payload) -> Vec<JourneyRun>` (`crates/flux-channels/src/deliver.rs:13-15`). **There
  is no way for a channel to call an operation**, which is exactly why `slack.rs` opens its own Slack
  client.
- The `send` op is not a substitute: it records the message and **prints only for a `cli` channel**
  (`crates/flux-app/src/ops.rs:154-167`, the `is_cli_channel` branch at `:163`). A journey that
  "replies" through `send` on a Slack channel writes to a log.
- The upstream data already exists and is validated: flux-connectors' `ChannelBinding`
  (`../flux-connectors/crates/connector-spec/src/inbound.rs:306`) carries transport, events,
  verification, discriminator, delivery id, payload map and reply; `../flux-connectors/providers/slack.toml:385,414`
  declares **both** of Slack's real transports from one event set, one payload map and one reply.

## Children

- **D-216** — the `connector` arm on `build_channels`: manifest → binding, every refusal at load
- **D-217** — the reply seam: `Deliverer::call_operation` + `App::call_op` through `Executor::dispatch`
- **D-218** — the reply wired to the connector Tool pack; `adapters/slack.rs` reduced to a transport
- **D-219** — operator allow-lists keyed on the binding's declared payload symbols
- **D-220** — Socket Mode as a transport under the binding driver
- **C-481…C-488** — generic declarative RFC 6455 bindings, guarded selected-system execution and
  durable Exchange subscriptions, with Asterisk ARI as the first proof

## Acceptance

- [ ] A program declares `channel support { kind = "connector", connector = "slack", binding =
      "events-api", … }` and receives Slack events, with **no vendor-specific Rust involved in the
      payload map, the discrimination, or the reply**.
- [ ] `build_channels` gains **one** arm. Adding a second vendor connector adds zero lines to
      `flux-channels`.
- [ ] **The reply is a tool call through `Executor::dispatch`.** flux's own contract — *"Every tool
      runs through `Executor::dispatch` … the dispatcher is the policy/approval/redaction gate"*
      (AGENTS.md) — holds for a channel reply exactly as for a model-issued call. A channel that could
      post to a vendor outside the executor is the defect this epic removes, not one it adds.
- [ ] **Every refusal flux-connectors makes at load, flux makes again against the manifest it reads.**
      A published artifact can be edited after publication; a webhook binding with no stated
      verification is refused on both sides or on neither.
- [ ] `crates/flux-channels/src/adapters/slack.rs` no longer contains a payload map, a reply, an
      allow-list or a dispatch — D-220 leaves only its connection loop.
- [ ] The gate is green in **both** workspaces.

## Notes

- **Design:** `../flux-connectors/docs/designs/connector-channel-seam.md` — the full seam, with every
  `path:line` in this epic verified at workspace 0.40.0 / commit `2abd0a13`. Its parent model is
  `../flux-connectors/docs/designs/channel-bindings.md`.
- **Depends on the verified-webhook seam** (`C-291` … `C-295`, epic `verified-webhook-channel`). That
  epic gives a webhook channel raw-body capture, one parameterized HMAC, a challenge hook, discriminator
  routing and a delivery envelope, all declared **by hand in the program**. This epic supplies the same
  parameters **from a published manifest** instead. Same verifier, two declaration sources — do not
  build a second one.
- **Depends on flux-connectors C-83** (bindings reach the manifest and `catalog.json`) and **C-115/C-117**
  (the Tool pack, for the reply). There is nothing to read and nothing to call before those land.
- **Scope boundary that must not drift.** flux keeps the transports and the time/lifecycle sources —
  `webhook`/`http`, `schedule`/`cron`, `startup`, `cli`, `a2a`. A connector describes a vendor; it never
  describes a loop. The consumer repository's vision forbids it shipping a runtime, and this seam is what
  keeps that true while still deleting the vendor code from here.
- **Generic RFC 6455 and Slack Socket Mode are different transports.** C-481's design adds a
  declarative socket connect block for ordinary service-relative handshakes such as Asterisk ARI.
  D-220 remains the vendor-specific Slack URL-ticket/envelope-ack loop that feeds raw JSON into this
  binding driver; it is not the generic handshake implementation.
- **Two Slack-specific things the connector side cannot describe yet**, both filed as findings in the
  design and both bounding this epic's first release: `EventDecl::when` cannot express *absence*, so Slack's
  `bot_id`/`subtype` loop guard is not reproducible and the `message` event stays unusable
  (`app_mention` works); and `ChannelBinding` has no `challenge` declaration, so `C-293`'s handshake
  hook has no manifest parameters to read — which is why D-220 (Socket Mode) is a child of this epic
  rather than an optional extra.
