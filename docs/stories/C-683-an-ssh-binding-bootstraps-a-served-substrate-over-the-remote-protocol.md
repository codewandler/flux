---
id: C-683
title: "An ssh binding bootstraps a served substrate over the remote protocol"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-system, flux-cli]
design: first-class-hosts
note: "Decision 0018 rule 3 composition: ssh is the bootstrap, never the substrate — the far side is still the flux binary serving the remote protocol, and verification-before-acceptance is unchanged"
---

# An ssh binding bootstraps a served substrate over the remote protocol

## Goal

The one substrate almost every operator already has is a machine with sshd and nothing else — a
dev box, a build machine, a homelab node. Today reaching it with flux means hand-provisioning
`flux system serve` there and declaring a `remote` binding. An `ssh` binding closes that gap as a
composition, not a transport: ssh's job is to get the served substrate running and reachable —
verify (or start) `flux system serve` on the far machine and forward its endpoint to a local
socket — and then everything rides the delivered remote protocol unchanged: bearer auth, version
negotiation, the guarded port, handshake admission. Mapping guarded operations onto raw
`ssh <cmd>` is explicitly rejected: it would substitute prose-over-ssh for the far-side capability
enforcement that makes a substrate trustworthy — the far side must still be the flux binary.

The local ssh client is an OS process and therefore goes through the single guarded spawn path
with pinned argv; host-key verification stays strict (no `StrictHostKeyChecking=no`, no
known-hosts bypass); the key is a credential *reference*, never a value in config or argv.

## Acceptance

- [ ] `ssh` joins the closed `HostBackend` vocabulary: declarable in `[[host]]` with
      `user@host[:port]`, an optional remote binary path, and a credential reference for the key;
      rendered by `flux host ls`/`show`; the unknown-key hard error unchanged; absent sshd, a
      usable key, or a startable far-side binary, resolution fails closed naming the missing
      piece — nothing ever falls back to running effects locally or to raw command execution.
- [ ] Resolution establishes the served endpoint over an ssh port-forward through the guarded
      spawn path (pinned argv, strict host-key checking, key by reference) and admits it with the
      standard remote-protocol handshake; the bearer token still authenticates over the tunnel —
      the tunnel never substitutes for protocol auth.
- [ ] `flux host probe` on an ssh binding performs the side-effect-free identity check through
      the tunnel and reports the negotiated protocol version and the far side's
      `SubstrateIdentity` with `remotely_reported` provenance; `browser.*`/`web.crawl` stay
      hidden under the selection (non-native kind, pinned like the sibling backends).
- [ ] A loopback-sshd integration test proves the full chain (bootstrap → forward → handshake →
      a guarded read on the far side) and the refusal faces (no sshd; host-key mismatch refuses
      rather than prompts; far-side version mismatch surfaces the protocol's own refusal),
      explicitly dispositioned where no sshd is available in CI.
- [ ] The config reference documents the binding, its bootstrap contract (what must exist on the
      far machine: the flux binary at a declared path or on PATH — installing it stays the
      operator's step, per the C-480 boundary), and the failure faces; the codegate censuses stay
      clean.

## Notes

- Serialize behind C-677 (microvm binding): same `host_cmd.rs`/`flux-capabilities` surfaces, and
  the microvm arm establishes the endpoint-composition pattern this story extends.
- The remote protocol serves TLS; the tunnel adds transport privacy but must not weaken the
  protocol's own auth or identity checks — reuse whatever loopback/pinned-identity form the
  delivered client already supports rather than inventing a bypass.
