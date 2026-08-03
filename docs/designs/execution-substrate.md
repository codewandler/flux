# Design: the execution substrate — `flux-system` for a second consumer

**Status:** partially shipped · **Pillar:** Core · **Epic:** [C-394](../stories/C-394-execution-substrate-epic.md)
· **Extends:** [portable-wasm-runtime.md](portable-wasm-runtime.md) (which introduced the port seam)
· **Context:** [ecosystem.md](ecosystem.md)

> Citations read on **2026-08-01** at workspace v0.42.0. Re-grep by symbol; line numbers move.

## Why now

`flux-system` has had exactly one consumer since it was written: flux. That is about to stop being
true — [flux-exchange](ecosystem.md) is a service that runs operations for many callers, and the
primitives it needs are the ones `flux-system` already guards: an HTTP request with a resolved-IP
egress guard, a dial, an argv-only spawn, an OS sandbox.

The wrong response is a new crate. The right response is to finish the seam `flux-system` already
has, because **the seam exists and was built for exactly this**: `port.rs` states the guarded
operations as capability traits so *"a WebAssembly embedder that answers through host imports, **a
remote executor**, or a test double"* can serve them, and it is explicit that the traits are
unsealed and that downstream implementors are expected.

## The distinction that must not be lost

The single most common misreading of this codebase, and the reason this design leads with it:

> **`flux-runtime` decides whether something may happen. `flux-system` is where it happens.**

They are both **L2** — peers at one layer, not stacked. Fusing them, or building a third crate that
spans them, would force every consumer of the substrate to also take flux's approval model. An
unattended service has no human at a terminal to prompt, so such a consumer would reimplement
guarded IO instead — which is the exact failure the substrate exists to prevent.

This is now stated publicly in [`docs/concepts.md`](../concepts.md); this design is the engineering
consequence.

## What is already there

| Need | Status |
|---|---|
| HTTP with SSRF guard | `flux-web` + `net::guard_url_scoped` — resolves hostnames, blocks private/loopback/link-local/ULA/CGNAT unless scoped-granted |
| TCP dial | `net::DialTarget` / `DialStream` / `dial_scoped` / `dial_scoped_pinned` |
| Argv-only spawn | one `System::build_command`; env cleared to a non-secret allow-list; output byte-capped |
| OS sandbox | `sandbox::Backend` — bubblewrap (Linux), Seatbelt (macOS); `SpawnPolicy`, `Confinement` |
| The port seam | `port::GuardedEnv`, `port::GuardedProcess`, `port::GuardedHostFiles`, `port::GuardedWorkspaceFiles`, `port::GuardedNetwork`, plus `ExecutionSystem`; fail-closed defaults; `flux-codegate` enumerates in-repo backends and pins its trait census to `port.rs` |
| A delegating backend | `remote::RemoteSystem` + `remote::Loopback` (C-399), with the authenticated HTTPS/WSS production transport from C-475/C-476 and four typed delivery outcomes |
| Published | yes — `codewandler-flux-system`, lib name `flux_system` |

So the substrate is real and shipped. What is missing is narrower than it looks.

### Guarded inbound is now the production serving path

`GuardedNetwork::bind_tcp` is the constructor for long-lived agent and channel listeners. The
standalone single-agent server derives that system from the `FlowEngine` executor; the multi-agent
server requires its host to pass the selected system explicitly. A2A, webhook and connector adapters
receive the same handle from their channel host. A remote-selected surface therefore cannot silently
open a local socket. `flux-server::GuardedHttpListener` adapts the opaque `NetworkListener` to axum
by pumping bytes through a bounded in-process duplex stream. The native or remote port still owns
every physical accept, read and write, including connection, frame and IO-time ceilings.

The bridge drives independently owned guarded read/write halves. This is part of the port contract,
not a native-socket shortcut: unread request bytes may backpressure their bounded half without
head-of-line blocking an SSE/streaming response. The authenticated remote-system wire multiplexes
one outstanding read and write and carries typed per-direction failures; dropping either protocol
end cancels the coordinator, closes the wire, and releases remote admission. That wire change is
remote-system protocol version 2.

The syntax-aware codegate census makes additions reviewable through ordinary aliases, fully-qualified
paths, and obvious macro bodies. It also refuses production APIs that name a native `TcpListener`
outside the native port, and treats direct `socket2`, libc and nix socket constructors as reviewed
network IO. Three direct-bind classes remain explicit rather than being mislabeled as guarded:

- two finite loopback OAuth callback handshakes, which accept one authorization code and close;
- the public-docs static server, which may intentionally be unauthenticated off-loopback and mounts
  no execution routes. `BindExposure` permits only loopback-open or authenticated public exposure,
  so claiming this listener is authenticated would weaken the contract;
- the one native constructor that implements `GuardedNetwork::bind_tcp` itself.

Tests may still bind native ephemeral listeners as fixtures; the census excludes `cfg(test)` code.
Like any source census, it does not expand procedural/build-script macros or inspect downstream
crates; those callers receive only the guarded public serving APIs and take responsibility for any
`ExecutionSystem` implementation they supply.

## What is missing

### 1. The workspace-confined file surface is a port

C-395 closed the old deferral: `GuardedWorkspaceFiles` now states the workspace-confined file
surface (`read_file`, `write_file`, …), with fail-closed defaults and the same confinement proof as
the concrete `System`. C-467 then brought that fourth trait into the codegate's reviewed-backend
enumeration and added a census against `port.rs`, so a future fifth `Guarded*` trait cannot be
silently omitted.

### 2. `DialTarget` covers TCP, not UDP or ICMP

Reachability checks, protocol probes and anything resembling `ping` need datagram and raw sockets.
These are new variants on existing enums rather than a new module —
[C-396](../stories/C-396-datagram-dial-targets.md). Raw ICMP additionally needs `CAP_NET_RAW`, which
is a deployment concern the design must state rather than discover: a capability the process may not
hold is a **refusal at construction**, not an error at first send.

**Resolved (C-396).** `DialTarget::Udp { host, port }` and `DialTarget::Icmp { host }` run through
`guard_target_host_pinned` — the same resolution and range checks TCP runs. There is no second
guard, and both sockets are `connect`ed to the vetted address, so the pin is enforced by the kernel
rather than by convention: no later call can address anything else.

The platform facts, and what flux does with them:

| Fact | Decision |
|---|---|
| Linux raw ICMP (`SOCK_RAW`/`IPPROTO_ICMP`) needs `CAP_NET_RAW`; macOS needs root | Refuse, naming the capability |
| Linux also offers unprivileged ICMP (`SOCK_DGRAM`/`IPPROTO_ICMP`, per-gid via `net.ipv4.ping_group_range`); macOS offers it to everyone | **No fallback.** It is a differently-privileged path whose wire semantics differ — the kernel owns and rewrites the echo identifier — so falling back would silently change what a probe measures depending on the host it ran on |
| A confined process (C-410's fail-closed sandbox, network closed) cannot reach the network | The `socket`/`connect` call fails at construction with its errno in the message; nothing is addressed and nothing is sent |
| Linux/BSD `socket(2)` accepts `SOCK_CLOEXEC`; macOS does not | Request it atomically where it exists. A descriptor marked close-on-exec by a *following* `fcntl` is inheritable for the width of that window, and this process spawns children concurrently while `Command` closes no inherited descriptors — so a `fork`+`exec` in the window hands a child a raw socket that traversed no grant. On macOS the `fcntl` remains and the window narrows but cannot be closed |

The privilege check is the `RawIcmpOpener` seam rather than a probe-then-open pair, because a
separate probe can disagree with the open that follows it. Opening and connecting a datagram socket
transmits nothing, which is exactly what lets the check sit at construction. The seam also keeps the
refusal *wording* in `net.rs`: an implementor reports the kernel's `io::Error` and this crate turns
`PermissionDenied` into the message naming `CAP_NET_RAW`, so no implementor can weaken it.

Neither variant is reachable from a plugin: `conn.dial` accepts no `kind` that builds one. Datagram
and raw egress stays outside the plugin surface until a manifest grant is designed for it.

### 3. Nothing says what binding `flux-system` *without* `flux-runtime` means

This is the sharpest gap and the one most likely to be got wrong quietly.

`AGENTS.md` states the invariant as *"Every tool runs through `Executor::dispatch`"*. That is a
statement about **flux**. A consumer that links `flux-system` and brings its own policy engine is
not violating it — it was never inside it — but nothing in the tree says so, and a reader who finds
the invariant and then finds a consumer bypassing `Executor` will reasonably conclude something is
broken.

`port.rs` answers the adjacent question (what it means to *implement* the port: *"a consumer that
implements these traits is taking responsibility for the guarantees itself"*) and not this one (what
it means to *consume* the substrate without the envelope).

[C-398](../stories/C-398-substrate-guarantee-contract.md) writes the contract: which guarantees are
`flux-system`'s and travel with it (path confinement, argv-only, egress guarding, sandbox
confinement, env clearing, output capping), and which are `flux-runtime`'s and **do not** (default-deny
authorization, approval, redaction of tool output, evidence). A consumer taking only the first set is
supported; a consumer that assumes it got the second is the failure this contract prevents.

### 4. Container and remote backends — ownership settled

A `container` runtime (spawn inside docker/k8s) and a `remote` runtime (delegate to another
substrate) are both named in [ecosystem.md](ecosystem.md)'s runtime table. The original design left
ownership open because the port is unsealed. That decision is now settled:

- **Flux owns both backends.** Local-first use must not depend on an out-of-repo service, while the
  unsealed port still lets other consumers provide their own implementations.
- The reviewed codegate allowance is an intentional cost: in-repo guarded-IO implementations must
  be visible to the repository's no-bypass checks.
- The CLI now uses the remote backend directly through explicit `--remote` selection.

**Resolved for the remote backend (C-399): flux owns it.** The alternative — leaving it to the first
consumer that needed it — would have put a locally-executing runtime behind a service, and flux must
be able to do this on a developer's own machine with nothing running. That is
[vision.md](../vision.md)'s local-first principle on the runtime axis, not a convenience.

`crates/flux-system/src/remote.rs` is the implementation: `RemoteSystem` serves all five port
families by handing each operation to a `Delegate`, and `Loopback` serves `Delegate` from any
in-process substrate — so the delegation path is exercisable with no service. It adds **no
dependency**: `Delegate` is a Rust trait, not a protocol, which is what keeps
[remote-agents.md](remote-agents.md)'s open question (is the remote wire a channel API or a port
delegation?) genuinely open. A wire format chosen here would have pre-answered it.

The out-of-crate test still proves that seam over bytes: it implements one operation using a
test-owned length-prefixed protocol over `tokio::io::DuplexStream`, verifies the request and response
cross the stream, and verifies a closed stream becomes `Unreachable`. The proof is real without
promoting its framing into a production protocol.

What the story turned out to be about is **three** failure modes rather than the two its Acceptance
named, because an operator's response to each is different and all three are ways one delegated
operation fails:

| Mode | What happened | What an operator does |
|---|---|---|
| `Refused` | The far side answered; the answer was no. A guard did its job. | Fix the request or widen the grant. Retrying unchanged is pointless. |
| `Unreachable` | No answer arrived. Whether the operation happened is **unknown**. | Investigate the link. Retrying is meaningful. |
| `Unserved` | The delegate does not implement the operation at all. | Implement it, or stop asking. Retrying never helps. |

The classification is **structural**: a delegate returns `Answer::Refused` or `Err(Unreachable)`,
which are different positions in the type, and only a transport can construct the latter. `settle`
stores the distinction in `flux_core::Error::GuardedIo` with a typed `GuardedIoFailure`, and
`failure_mode` matches that variant rather than formatted text. A refusal whose reason begins with
the exact unreachable diagnostic therefore still classifies as a refusal. That
matters more than it looks: delegate-authored text that could reclassify a refusal would send an
operator to investigate a perfectly healthy network.

The container backend (C-397) remains open, but its ownership is no longer open: Flux owns it and
external consumers may reuse it.

## What this epic explicitly does not do

- **It does not move flux-runtime.** No change to `Executor::dispatch`, the approval chain, or the
  layer map.
- **It does not add an IO path.** Every item above is either a trait over the existing path or a new
  variant inside it. A second `Command::new` or a second URL guard would be a defect, not a feature.
- **It does not weaken a default.** Port operations default to denial; new ones must too.
- **It does not build flux-exchange.** That lives in its own repository. This epic makes the
  substrate consumable; it does not consume it.

## The check that keeps this honest

Each story's Acceptance names a failing-first test. Two are worth calling out because they are the
ones a well-meaning implementation would skip:

- C-395 must show that a **port-based** consumer is confined exactly as a concrete `System` consumer
  is — the same escape attempts (`..`, a symlink out of the root) refused through the trait.
- C-396 must show that a raw-ICMP target with insufficient capability is refused **at construction**,
  not at first send. A capability check that happens on the wire is a check that already leaked the
  attempt.
- C-399 must show that a delegate **cannot forge a failure mode with its wording** — the check a
  well-meaning implementation skips because it tests its own two fixtures against each other and
  both agree. A refusal reading "unreachable" that classifies as unreachable would be a backend that
  is worse than none: it sends an operator to investigate a healthy link on a delegate's say-so.
