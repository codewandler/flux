---
id: A-135
title: "`A2aClient` hardwires its transport, so there is no socket-free way to test a fleet journey"
pillar: Agent
status: backlog
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [flux-a2a, flux-orchestrate]
note: "A-117's blocker B3, the only one of its five with no owner — every fleet test today binds a real loopback listener because A2aClient offers no injectable-transport seam"
---

# `A2aClient` hardwires its transport, so there is no socket-free way to test a fleet journey

## Goal
A-117's headline Acceptance is an **offline** end-to-end cycle against `MemoryBoard` and a "stub A2A
worker, no credentials, no network". That is not currently satisfiable without a socket.

`A2aClient` builds its transport internally: the struct holds `http: reqwest::Client`
(`crates/flux-a2a/src/client.rs:44`) and `A2aClient::new` fills it with `reqwest::Client::new()`
(`client.rs:70`). The builder surface is `with_token` (`:79`), `with_header` (`:85`) and
`with_rpc_url` (`:91`) — configuration of the request, not of the transport. There is no seam to
inject a stub, and `FleetDispatchTool`'s spawner constructs the client from an endpoint string
(`crates/flux-orchestrate/src/fleet.rs:149`), so a test cannot hand one in from outside either.

The consequence is visible in the tests that exist: `fleet.rs`'s `worker_stub` (`fleet.rs:518-556`)
binds a real loopback listener, and every `fleet.*` test goes through it (`fleet.rs:593`, `:632`,
`:652`, `:677`, `:808`, `:834`). The fleet board-recovery test written during the integration run
(`crates/flux-sdk/tests/fleet_board_recovery.rs`) does the same thing. `flux_a2a::server::dispatch`
is socket-free but is not a usable substitute: it implements `message/send` only and explicitly
classifies `tasks/get` and `tasks/cancel` as unsupported
(`crates/flux-a2a/src/server.rs:177`, `:195-207`), which is exactly the half `fleet.status` and
`fleet.cancel` need.

**This story does not pre-decide the approach.** There are two honest options and they prove different
things:

- **(a) A loopback listener.** Exercises the real `reqwest` path, real JSON-RPC framing, real HTTP
  status handling, and the actual `A2aClient` code a production dispatch runs. It is not socket-free:
  it binds a port, so it is subject to sandboxing, CI port pressure, and the timing flake class that
  comes with real sockets. "No network" becomes "no *remote* network".
- **(b) Stub at the op boundary.** Inject test `fleet.*` tools via `App::try_with_tools`
  (`crates/flux-app/src/app.rs:191`) and never construct an `A2aClient` at all. Genuinely socket-free
  and fast, and it proves the journey wiring — intake, claim, dispatch, sweep, transition. It proves
  **nothing** about `A2aClient`, the A2A wire, or how a real worker's task states map onto board
  states, which is where the interesting bugs live.

A third possibility is that the right answer is an injectable-transport seam on `A2aClient` itself, so
(b)'s speed and (a)'s coverage stop being mutually exclusive. Choosing among the three — and stating
what the chosen test is therefore *entitled to claim* — is this story's actual work.

## Acceptance
- [ ] The choice is made and justified in this story's Progress: loopback listener, op-boundary stub,
      or an injectable transport on `A2aClient`. The justification must state what the resulting test
      proves and, explicitly, what it does not.
- [ ] Failing-first test: a fleet journey — dispatch through to a worker-reported terminal state —
      runs under the chosen mechanism. It must fail on today's tree for a substantive reason (the seam
      or the harness does not exist), not merely because a helper is unwritten.
- [ ] If a transport seam is added to `A2aClient`: it does not weaken the egress posture. The worker
      endpoint is caller-supplied and therefore model-reachable, and A-116 resolves it through
      `guard_url_scoped` before any request — a seam that lets a caller bypass the guard is a
      regression, not a testing convenience. Failing-first test that the guard still runs on the
      seam'd path.
- [ ] If the op-boundary stub is chosen: the gap is recorded where it will be read, not just in this
      story — `crates/flux-orchestrate/src/fleet.rs` already carries the note about
      `server::dispatch`'s unsupported methods (`fleet.rs:16-18`), and "the end-to-end test does not
      exercise the A2A wire" belongs beside it.
- [ ] Whatever lands is usable by **A-117** as-is: A-117's Acceptance names "a stub A2A worker, no
      credentials, no network", and its B3 says which one counts "is a review criterion this story
      must state before the test is written". Settling that is this story's deliverable, so A-117's
      test can be written against a decided rule rather than re-litigating it.
- [ ] Existing `fleet.*` tests keep passing — if a new mechanism supersedes `worker_stub`, the
      migration is deliberate and the wire coverage `worker_stub` currently provides is not silently
      dropped.
- [ ] Standard gate green in both workspaces (root + `plugins/`), `cargo fmt --check` included.

## Progress
- (not started)

## Notes
- Filed 2026-07-29 from the fleet-coordinator integration run. This is **A-117's blocker B3**
  (`docs/stories/A-117-coordinator-program.md`, "B3 — a socket-free A2A worker stub does not exist"),
  surfaced by that story's blocked-at-pickup analysis, and it is **the only one of A-117's five
  blockers with no owner**: B1 → A-130, B2/B4 → A-131, B5 → C-232, B3 → (this story).
  `docs/stories/A-117-coordinator-program.md` is `status: blocked` on it.
- Evidence as given by A-117's implementor and re-verified against `main` at base `9721daca`:
  `A2aClient` constructs `reqwest` internally with no injectable-transport seam
  (`crates/flux-a2a/src/client.rs:44`, `:70`), so a fleet test must bind a real loopback listener —
  which is what `crates/flux-orchestrate/src/fleet.rs`'s `worker_stub` (`:518-556`) and
  `crates/flux-sdk/tests/fleet_board_recovery.rs` both do.
- ⚠ `crates/flux-sdk/tests/fleet_board_recovery.rs` is not on `main` at filing (base `9721daca`); it
  comes from the integration run's in-flight A-131 work. Check for it before citing it as precedent.
- The A-112 rendezvous pattern (`crates/flux-app/tests/integration.rs:770-835`) is the reference for
  keeping whichever harness is chosen deterministic rather than sleep-timed.
