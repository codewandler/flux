---
id: C-435
title: "The port has no network trait — and no guarded inbound primitive at all"
pillar: Core
status: in-progress
priority: 4
design: docs/designs/execution-substrate.md
epic: execution-substrate
areas: [flux-system]
note: "⚠ measured 2026-08-01: port.rs declares FOUR traits — GuardedEnv, GuardedProcess, GuardedHostFiles, GuardedWorkspaceFiles — and NONE for network. Egress guarding lives in net.rs as free functions; inbound binding is scattered (flux-server's guard_open_bind, and C-409 found the adapters that bind their own listeners got none of its hardening). The prerequisite for embedding any protocol stack under flux's guard"
---

# The port covers env, process and files — not the network

## Goal

State the guarded network operations as port traits, including **inbound**, so a substrate or an
embedded protocol stack can be served by flux's guard rather than opening its own sockets.

## What the code said when filed

`crates/flux-system/src/port.rs` declares exactly four traits: `GuardedEnv`, `GuardedProcess`,
`GuardedHostFiles`, `GuardedWorkspaceFiles`. **There is no network trait.** Egress guarding lives in
`net.rs` as free functions and the `DialTarget` enum — `guard_url_scoped`, `dial_scoped_*` — which is
fine for flux's own callers and unusable by a consumer that must be *handed* its IO.

⚠ **Inbound is worse than absent — it is scattered.** `flux-server` has `guard_open_bind`; the channel
adapters bind their own listeners and, per [C-409](C-409-channel-served-http-has-no-resource-limits.md),
*"got none of it"* — no body caps, timeouts, rate limits or concurrency admission. There is no single
place that says what it means to accept a connection under flux's guarantees.

## Why now — the second consumer arrived

This epic exists for exactly this condition. C-395's reasoning is on record: C-269 deferred the file
port because *"a trait with no call sites would be indirection without a seam"*, and **a second
consumer is the condition that expires that reasoning**.

[D-230](D-230-the-native-sip-backend.md) is that consumer. Embedding a SIP/RTP stack under flux's guard
needs precisely: resolve a name, dial UDP and TCP, **and bind a local port to receive** — RTP is
bidirectional and inbound SIP needs a listener. The first two are nearly there ([C-396](C-396-datagram-dial-targets.md)
landed guarded UDP dial today, vetting the destination once and `connect`ing so the kernel enforces
both directions). ⚠ **The third does not exist.**

## Acceptance

- [x] **Failing-first**: a test driving guarded network operations through the port with a substituted
      implementation — failing at the merge base because the trait does not exist.
- [x] A network port following `port.rs`'s stated shape: ⚠ **no god trait** — *"the port is split by
      guarded resource, and a consumer names only the traits it uses"* — and a consumer spanning
      families *"declares its own bundle (see `flux_plugin::PluginSystem`)"*. Follow that precedent
      rather than inventing a second convention.
- [x] ⚠ **Nothing relaxes a guarantee.** `port.rs` is explicit: *"This is not a second IO path… The port
      makes the caller substitutable, not the guard."* A port implementation must gain no ability the
      caller did not already have. This is the sentence to test against, not merely to cite.
- [x] The outbound side delegates to the existing `net.rs` guard — `guard_url_scoped`,
      `guard_target_host_pinned`, `DialTarget` — with **no second range check, hostname rule or
      allowlist derived anywhere.** `AGENTS.md` forbids a second URL guard and C-396's review confirmed
      the shared-helper discipline.
- [ ] ⚠ **Inbound is defined, and it is the new part.** What it means to bind and accept under flux's
      guarantees, in one place — so C-409's finding (adapters binding listeners with none of
      flux-server's hardening) becomes structurally impossible rather than repeatedly fixed.
- [x] Resolution is a seam: C-396's tests already inject a `FixedResolver`/`RebindingResolver`, so the
      shape exists — lift it rather than deriving a third.
- [ ] Full gate green.

## Notes

- ⚠ Interacts with the [network-primitives](C-418-guarded-network-primitives-epic.md) epic (C-284…C-288),
  which is about the *op surface* a model can call. This is the *substrate* beneath it. C-284 ("design
  guarded network primitives — the shape the rest inherit") should be read before this lands, so the two
  do not describe the same thing twice — the boundary C-418 was filed to state.
- The likely first external consumer is `sipx` ([D-230](D-230-the-native-sip-backend.md)), which already
  exports `resolve::{Naptr, Resolver, Srv}` and `endpoint::{Config, Handle, bind}` as concrete types —
  the seams exist, they are simply not injectable yet.
- ⚠ RTP binds a local port per call and receives from a remote that may differ from the one dialled
  (symmetric RTP mitigates, does not eliminate). An inbound design that assumes "accept a connection"
  will not fit datagram media; check it against that case before settling the shape.

## Progress

- Filed 2026-08-01, after the SIP epic surfaced the gap.
- 2026-08-02: `GuardedNetwork` now returns opaque bounded stream/listener/datagram resources. Native
  outbound uses the existing pinned-address guard; inbound requires loopback or authenticated
  exposure and carries connection/frame/timeout limits. Remote delegation and HTTPS/WSS transport
  exercise all three resource shapes. The remaining unchecked item is migrating older server and
  channel listeners so C-409's scattered-bind class becomes structurally impossible everywhere.
