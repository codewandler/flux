---
id: D-216
title: "`build_channels` gains one `connector` arm — a manifest binding, with every rule a load error"
pillar: Agent
status: in-progress
epic: connector-channels
note: "the arm itself: resolve ~/.flux/connectors/<name>.connector.toml through flux_system, load the named binding, and refuse EVERYTHING refusable before a port is bound — a manifest is a published artifact and a published artifact can be edited after publication"
---

# `build_channels` gains one `connector` arm — a manifest binding, with every rule a load error

## Goal

Add the single generic arm: `kind = "connector"` resolves a connector manifest, loads the named channel
binding out of it, and constructs a channel that drives it. Every rule the producing repository enforces
at compile time is enforced **again** here against the file actually on disk, before any listener binds.

This is the foundation of the epic. D-217 … D-220 all layer on the channel this story constructs.

## Context — verified against this tree

- `build_channels(&[ChannelDecl])` is decl-only and synchronous
  (`crates/flux-channels/src/adapters/mod.rs:36`); the `match` at `:46` is closed and `:63` makes an
  unknown kind a hard error. This story adds one arm, beside `"webhook" | "http"`.
- `ChannelDecl` is `{ name, kind, settings }` with a free-form JSON `settings` bag
  (`crates/flux-lang/src/program.rs:76`), so **no language change is needed** — the same route
  `WebhookSettings` takes (`crates/flux-channels/src/config.rs:18-32`).
- Secrets are already resolved before any adapter deserializes: `resolve_secrets` walks
  `program.channels`, registers each value with the redactor and substitutes it, and `build_channels`
  refuses any surviving `{"$secret":…}` marker (`crates/flux-channels/src/adapters/mod.rs:23-32`,
  refused at `:39-45`, test at `:75`). A `credentials { … }` sub-record inherits that for free — see
  `C-291`'s finding that `parse_setting_prefix` recurses, so `secret "NAME"` is recognised at any
  nesting depth.
- `a2a` is the precedent for a check that needs the live `App`: it is built in `serve` rather than in
  the decl-only builder (`crates/flux-channels/src/adapters/mod.rs:62`,
  `crates/flux-channels/src/host.rs:37-40`).
- The binding schema being read: `ChannelBinding`
  (`../flux-connectors/crates/connector-spec/src/inbound.rs:306`) — `transport`, `events`,
  `verification`, `discriminator`, `delivery_id`, `payload`, `reply`, `cursor`, `interval`.

## Acceptance

- [ ] Settings: `connector` (required), `binding` (required), optional `service`, optional `manifest`
      path override, plus the transport's own settings (`addr`/`path` for `webhook`) and a
      `credentials { "<name>" = secret "ENV" }` record mapping every credential the binding names.
- [ ] The manifest resolves to `~/.flux/connectors/<connector>.connector.toml` — read **through
      `flux_system::System`**, never `std::fs`, mirroring how `~/.flux/flows` is already the home for a
      connector's `.flux` module (`crates/flux-tools/src/flows.rs:26`).
- [ ] **Failing-first test `unverified_webhook_binding_is_refused_at_load`**: a manifest whose
      `webhook` binding states no verification makes `build_channels` return `Err`, and **no port is
      bound**. Assert on the constructor's result, not on a request. This is the producing repository's
      own refusal, reproduced against the file — the point of the story is that flux cannot be the
      bypass.
- [ ] Every one of these is a **load error**, each with a test, each naming the channel:
      - no manifest for `connector`, or no binding named `binding` in it;
      - a `transport` this arm cannot serve (`poll` needs a `schedule` channel and a `trigger`, not this);
      - `verification` unset on a `webhook` binding;
      - a credential the binding names with no `credentials` entry (otherwise the signature check fails
        open, or the reply 401s on first delivery);
      - a `signed` template interpolating `{timestamp}` with no `tolerance`, or with a **body-sourced**
        timestamp selector — the same refusals `C-291` makes;
      - a `reply.bind` naming a payload symbol the `payload` map does not declare;
      - a `payload` path that fails the dotted-path grammar.
- [ ] **The reply operation's tool must exist**, asserted in `serve` before any channel task is
      spawned — it needs the registry (`crates/flux-app/src/app.rs:458`), which the decl-only builder
      does not have. Test `missing_reply_tool_refuses_at_startup`. Follow `a2a`'s split; do not smuggle
      an `Arc<App>` into `build_channels`.
- [ ] **The manifest is untrusted input for path purposes.** No field read out of it may influence a
      filesystem path, and the `connector` setting is validated against a name grammar before it is
      joined onto the connectors directory. A `connector = "../../etc"` must be refused, not resolved.
      Test `connector_name_cannot_traverse`.
- [ ] The channel's delivery path is concurrent-safe with no serialization of its own: deliveries run
      concurrently and are bounded by the App's admission limit (`crates/flux-channels/src/lib.rs:19-38`,
      `crates/flux-app/src/app.rs:503-513`), so an adapter that must not block its protocol loop spawns
      its `deliver` call.
- [ ] Adding a second connector adds **zero** lines to `flux-channels`. Asserted by a test that drives
      the arm from a second, differently-shaped fixture manifest.

## Progress

- (not started)

## Notes

- Parent: **D-215**. Design: `../flux-connectors/docs/designs/connector-channel-seam.md`, sections
  "Where the manifest is read from" and "`build_channels` gains one arm, not one per vendor".
- **Depends on `C-291`** for raw-body capture and the verifier this arm feeds parameters to, and on
  flux-connectors **C-83** for bindings actually reaching the manifest. Do not start before both.
- Deliberately **not** in this story: the reply (D-217/D-218), allow-lists (D-219), Socket Mode (D-220).
  This story ends at "an event reaches a trigger".
- The routing rule this arm applies is `C-294`'s, **narrowed**: because `ChannelBinding::events` is a
  closed set, a discriminator value outside it is a logged no-op rather than a sanitised label and never
  falls back to the bare channel name. Otherwise a vendor names this host's trigger labels.
- `EventDecl::when` is matched by **`const` equality only** in v1 — enough for GitHub's single `issues`
  event narrowing to `issues.opened`. Absence matching is not expressible in the upstream schema at all
  and is filed as a finding on the producing repository, not worked around here.
