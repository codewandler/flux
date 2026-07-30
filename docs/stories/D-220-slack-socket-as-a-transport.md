---
id: D-220
title: "Socket Mode becomes a transport under the binding driver — the last 40 lines of the Slack adapter"
pillar: Agent
status: backlog
epic: connector-channels
note: "a WebSocket handshake is a vendor protocol and no manifest can describe one, so the slack-morphism dependency does not vanish — but everything ABOVE the connection does. This is what makes 'slack.rs can be deleted' honest instead of a claim that quietly costs operators their NAT-friendly deployment"
---

# Socket Mode becomes a transport under the binding driver — the last 40 lines of the Slack adapter

## Goal

Port Slack Socket Mode to a **transport** that feeds the generic binding driver, so the Slack adapter
stops being a channel kind and becomes a connection loop with no knowledge of payloads, replies or
policy.

## Context — verified against this tree

- Socket Mode is a vendor WebSocket protocol. `crates/flux-channels/src/adapters/slack.rs:56-82`
  constructs a `SlackClient`, a listener environment and a push callback via `slack-morphism`
  (`crates/flux-channels/Cargo.toml`, optional, default feature `slack`). **No manifest can describe
  that handshake**, so this dependency is not deletable.
- Everything else in the file is describable and is deleted by D-218: the payload map
  (`:172-180`), the reply (`:150-161`), the allow-list (`:183-187`), the event-type dispatch
  (`:97-127`).
- The upstream binding already declares the socket transport as a peer of the webhook one:
  `../flux-connectors/providers/slack.toml:385-410` — `transport = "socket"`, the same two events, the
  same payload map, the same reply, and **no verification**, because nothing arrives unsolicited over a
  connection we opened and authenticated.
- The upstream `Transport` enum names all three (`webhook`, `socket`, `poll`) precisely so that
  "inbound" is an abstraction over transports rather than a synonym for "webhook"
  (`../flux-connectors/crates/connector-spec/src/inbound.rs:54-66`).

## Acceptance

- [ ] `crates/flux-channels/src/adapters/slack.rs` becomes a **transport** (e.g.
      `transports/slack_socket.rs`), still feature-gated on `slack`, exposing one thing: a loop that
      yields raw event JSON and a cancellation path. It holds no payload map, no reply, no allow-list,
      no deliverer call of its own.
- [ ] `kind = "slack"` is **removed from `build_channels`** (`crates/flux-channels/src/adapters/mod.rs:49-58`).
      A program still using it gets a clear migration error naming the `connector` replacement — not the
      generic "unknown channel kind" at `:63`.
- [ ] A `connector` channel whose binding declares `transport = "socket"` runs over this transport and
      produces the **same deliveries and the same replies** as the same binding's `webhook` transport.
      **Failing-first test `both_slack_transports_drive_one_binding`** — the property flux-connectors
      already proved at the IR level, now proved at runtime.
- [ ] The socket transport declares **no verification** and the arm accepts that, because the upstream
      tri-state distinguishes "unverifiable, stated deliberately" from "unstated"
      (`../flux-connectors/crates/connector-spec/src/inbound.rs:173`). A `webhook` binding with the same
      silence is still refused (D-216). Assert both halves.
- [ ] The `slack` feature still gates only the vendor SDK. `cargo build -p flux-channels
      --no-default-features` builds, and a `connector` channel with a `webhook` binding works without it.
- [ ] Roughly 40 of the original 217 lines survive. State the real number in Progress; it is the story's
      own evidence.

## Progress

- (not started)

## Notes

- Parent: **D-215**. Depends on **D-218** (which does the deleting). Design:
  `../flux-connectors/docs/designs/connector-channel-seam.md`, section "So: yes, but only via the socket
  transport".
- **Why this is a child of the epic and not an optional extra.** The webhook transport cannot replace
  Socket Mode yet: Slack's Events API registration requires answering a `url_verification` handshake, and
  while flux's `C-293` implements the hook, the **upstream binding has no `challenge` declaration** to
  feed it (`ChannelBinding`, `../flux-connectors/crates/connector-spec/src/inbound.rs:306`). Until that
  gap closes, deleting Socket Mode would take away the deployment that works today — no public URL, no
  inbound port, works behind NAT — and offer one that cannot be registered.
- The tradeoff to state in the docs, because it is now an operator's choice rather than flux's:
  Socket Mode needs an app-level token and no public URL; the Events API needs a public URL and a signing
  secret. `../flux-connectors/providers/slack.toml:373-375` already says exactly this, and it is the
  clearest single statement of why both transports exist.
