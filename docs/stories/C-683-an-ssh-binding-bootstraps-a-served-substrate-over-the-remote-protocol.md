---
id: C-683
title: "An ssh binding bootstraps a served substrate over the remote protocol"
pillar: "Core"
status: done
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

- [x] `ssh` joins the closed `HostBackend` vocabulary: declarable in `[[host]]` with
      `user@host[:port]`, an optional remote binary path, and a credential reference for the key;
      rendered by `flux host ls`/`show`; the unknown-key hard error unchanged; absent sshd, a
      usable key, or a startable far-side binary, resolution fails closed naming the missing
      piece — nothing ever falls back to running effects locally or to raw command execution.
- [x] Resolution establishes the served endpoint over an ssh port-forward through the guarded
      spawn path (pinned argv, strict host-key checking, key by reference) and admits it with the
      standard remote-protocol handshake; the bearer token still authenticates over the tunnel —
      the tunnel never substitutes for protocol auth.
- [x] `flux host probe` on an ssh binding performs the side-effect-free identity check through
      the tunnel and reports the negotiated protocol version and the far side's
      `SubstrateIdentity` with `remotely_reported` provenance; `browser.*`/`web.crawl` stay
      hidden under the selection (non-native kind, pinned like the sibling backends).
- [x] A loopback-sshd integration test proves the full chain (bootstrap → forward → handshake →
      a guarded read on the far side) and the refusal faces (no sshd; host-key mismatch refuses
      rather than prompts; far-side version mismatch surfaces the protocol's own refusal),
      explicitly dispositioned where no sshd is available in CI.
- [x] The config reference documents the binding, its bootstrap contract (what must exist on the
      far machine: the flux binary at a declared path or on PATH — installing it stays the
      operator's step, per the C-480 boundary), and the failure faces; the codegate censuses stay
      clean.

## Notes

- Serialize behind C-677 (microvm binding): same `host_cmd.rs`/`flux-capabilities` surfaces, and
  the microvm arm establishes the endpoint-composition pattern this story extends.
- The remote protocol serves TLS; the tunnel adds transport privacy but must not weaken the
  protocol's own auth or identity checks — reuse whatever loopback/pinned-identity form the
  delivered client already supports rather than inventing a bypass.

## Design

The binding is a **composition of two delivered things**, not a new substrate. Nothing implements a
guarded port for `ssh`: what resolves is the delivered `RemoteSystem`, connected by the delivered
`HttpDelegate`, over an ssh port-forward it happens to be holding open.

**Bootstrap → attach → handshake** (`crates/flux-server/src/ssh.rs`):

1. Reserve a loopback port through `GuardedNetwork::bind_tcp` — the one reviewed native listener
   constructor — and release it.
2. Spawn `ssh -N -L 127.0.0.1:<local>:127.0.0.1:<serve_port>` through `System::spawn_background`,
   the guarded process path. Wait, bounded, for the local end to accept. A client that dies first is
   diagnosed from its own words into `Unreachable` / `HostKeyMismatch` / `AuthRefused` / `NoKey`.
3. Handshake through the forward with the binding's bearer token. Admitted → **attached**.
4. Refused: if the far side *answered* (an `Error::Config` version refusal, or an HTTP status) the
   protocol's own refusal is surfaced verbatim — starting a serve cannot change either answer. If
   nothing answered, `AttachOnly` (every `probe`, and any binding with no far-side `cert`/`key`)
   refuses naming the missing piece; `VerifyOrStart` runs one pinned remote command —
   `<binary> system serve --bind 127.0.0.1:<serve_port> --cert … --key …` — over a second session
   and re-handshakes until a bounded deadline.
5. `RemoteSystem::tethered(tunnel)`: the substrate **owns** the tunnel, so it is released exactly
   when the substrate is, on every path.

**Idempotency has no lock.** The far side's `--bind` is the mutex: a second local session that
starts a serve while one is listening loses the bind, its child exits, and its next handshake
attaches to the winner. Nothing in flux reserves or reaps a far-side process, so the failure mode is
"one attempt exits", never "one session kills the other's substrate".

**Where the pieces live.** `crates/flux-system/src/ssh.rs` owns the local half — the pinned argv,
the live `SshTunnel`, the typed `SshRefusal`s — because the ssh client is an OS process and that is
the crate that spawns them. `crates/flux-server/src/ssh.rs` owns the state machine, because the
protocol client lives there. `crates/flux-cli/src/ssh_host.rs` resolves a `HostRef` into a plan,
turning both credential *references* into a key path and a token. The three `host_cmd.rs` arms are
thin.

**Two consequences worth stating.** `probe` never starts a far-side serve — the family defines a
probe as side-effect-free, and launching a process on someone's build machine is an effect; and
`-F none` means neither ssh config file is read, so the `[[host]]` entry is the whole declaration
rather than something a file the binding never named can change.
