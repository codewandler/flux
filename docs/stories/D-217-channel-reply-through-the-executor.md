---
id: D-217
title: "A channel can call an operation — `Deliverer::call_operation` through the full safety envelope"
pillar: Agent
status: backlog
epic: connector-channels
note: "the crux: Channel::start gets ONE seam, `Arc<dyn Deliverer>`, whose only method is deliver() — so a channel that wants to answer an event has to open its own client, which is exactly what adapters/slack.rs does at :150-161. That is a second, unpoliced request path inside the process"
---

# A channel can call an operation — `Deliverer::call_operation` through the full safety envelope

## Goal

Give a channel adapter a way to invoke a **registered operation** — and make that way go through
`Executor::dispatch`, so a channel reply is gated exactly like a model-issued tool call.

This is the crux of the epic. If a channel cannot answer an event through flux's own executor, a
connector binding buys nothing: the reply has to be hand-written adapter code again, and it has to open
its own socket to do it.

## Context — verified against this tree

- `Channel::start(&self, d: Arc<dyn Deliverer>, cancel)` — `crates/flux-channels/src/channel.rs:22`.
  That `Deliverer` is the **only** seam an adapter has, and it has exactly one method:
  `deliver(&self, label, payload) -> anyhow::Result<Vec<JourneyRun>>`
  (`crates/flux-channels/src/deliver.rs:13-15`).
- So `adapters/slack.rs` builds its own `SlackClient`, holds its own bot token, and posts the reply
  itself (`:56-58`, `:150-161`). **That call is not dispatched.** It consults no permission rule, raises
  no intent, and reaches no approver.
- The contrast is flux's own stated rule: *"Every tool runs through `Executor::dispatch`
  (`flux-runtime`). Don't call a tool's `execute` directly outside tests; the dispatcher is the
  policy/approval/redaction gate"* (`AGENTS.md`, non-negotiable conventions). `Executor::dispatch` is
  documented as *"Run a tool call through the full safety envelope"* (`crates/flux-runtime/src/lib.rs:3558`).
- `send`/`ask` do not fill the gap: `send` records the message and prints **only** for a `cli` channel
  (`crates/flux-app/src/ops.rs:154-167`, `:163`).
- The machinery to build a scoped executor already exists: journeys derive theirs from the App's shared
  `ExecutionEnvironment` (`crates/flux-app/src/app.rs:1605-1626`,
  `ExecutionEnvironment::into_executor` at `crates/flux-runtime/src/lib.rs:2579`), replacing only the
  registry and the permission rules.
- Layering permits it: `flux-channels` is L6, `flux-runtime` is L2
  (`crates/flux-codegate/src/lib.rs:44`, `:53-54`). Today `flux-runtime` is only a **dev**-dependency of
  `flux-channels` (`crates/flux-channels/Cargo.toml`), so this promotes it.

## Acceptance

- [ ] `Deliverer` gains **one defaulted method**:
      `async fn call_operation(&self, op: &str, params: Value) -> anyhow::Result<Value>`, defaulting to
      an error. Defaulted so every existing test double compiles unchanged, and so a deliverer that
      cannot dispatch **says so loudly** rather than dropping the reply silently.
      `Channel::start`'s signature does not change.
- [ ] The seam is typed in `serde_json::Value` + `anyhow::Result`, mirroring `deliver`, so
      `flux-channels` never names `ToolResult` and the new dependency stays shallow.
- [ ] `AppDeliverer` implements it over a new `App::call_op`, which derives an executor from the App's
      shared `ExecutionEnvironment` and calls `Executor::dispatch`. **Failing-first test
      `channel_reply_is_dispatched_not_executed`**: a recording tool asserts it was reached through
      `dispatch` — i.e. its `permission_subjects` and `intents` were consulted — and the test fails
      against an implementation that calls `Tool::execute` directly.
- [ ] **The allow-list is exactly one op.** The executor built for a reply grants **only** the operation
      the caller named; host `deny` rules still win and the App's `Approver` is unchanged. Test
      `channel_reply_cannot_reach_a_second_op`: a reply naming `command.invoke` is denied even though a
      journey on the same App may call it. Without this, installing a connector whose manifest names a
      process-spawning op as its reply is remote code execution behind a webhook.
- [ ] It is **not** a journey's envelope. A reply must not inherit `LEGACY_JOURNEY_ALLOW`
      (`crates/flux-app/src/app.rs:1611-1618`) — assert the negative, because inheriting it is the
      easy accident.
- [ ] An approval-gated or denied reply is reported, not swallowed: the failure is logged with the
      channel name and the op name, and never with the params (they carry the payload). Compare
      `slack.rs:160`, which `eprintln!`s the raw error.
- [ ] `flux-runtime` moves from `[dev-dependencies]` to `[dependencies]` in
      `crates/flux-channels/Cargo.toml`, and `cargo test -p flux-codegate` stays green.

## Progress

- (not started)

## Notes

- Parent: **D-215**. Design: `../flux-connectors/docs/designs/connector-channel-seam.md`, section
  "The reply is an operation call — this is the crux".
- **Useful beyond connectors.** Any channel that needs to answer out-of-band gets a policed route by
  this story alone; the `room` kind (D-204) is the obvious next consumer.
- **Rejected alternative — a new parameter on `Channel::start`.** It changes every adapter's signature
  and every test double for one adapter's need. A defaulted trait method is additive and self-documenting
  at the call site.
- **Rejected alternative — a `channel.reply` op a journey calls.** It moves the binding's outbound half
  into every operator's flow (the hand-maintained integration this epic removes) and hands the reply a
  journey's full grants instead of one op's.
- **Rejected alternative — `flow_run` over the connector's stored composite** (`crates/flux-tools/src/flows.rs`,
  which runs a stored flow *"through the engine's depth-guarded authored flow host, so it inherits the
  approval + IO envelope"*). A real envelope, and it needs no Tool pack — but a composite cannot make a
  live vendor call until flux gains the outbound `$auth` marker capability, and a stored flow resolves by
  file stem rather than by a registry name that can be scoped to exactly one entry. Recorded as the
  fallback, not the plan.
