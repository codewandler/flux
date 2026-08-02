# Design: Remote agents — run the agent here, land the effects there

**Status:** shipped (v1, port-aware catalog) · **Pillar:** Core · **Stories:** [C-436](../stories/C-436-flux-tui-remote.md) · [C-437](../stories/C-437-which-guarantees-travel.md) · [C-438](../stories/C-438-where-do-the-files-live.md) · [C-439](../stories/C-439-trusting-a-remote-substrate.md) · [C-440](../stories/C-440-the-topologies-page.md) · [C-473](../stories/C-473-remotely-representable-guarded-resources.md) · [C-474](../stories/C-474-selectable-execution-system.md) · [C-475](../stories/C-475-remote-system-https-protocol.md) · [C-476](../stories/C-476-remote-operation-delivery.md) · [C-477](../stories/C-477-execution-placement-and-deployment-guide.md) · [C-478](../stories/C-478-explicit-operation-execution-placement.md) · [C-479](../stories/C-479-plugins-on-the-selected-execution-system.md) · [C-480](../stories/C-480-first-class-remote-system-deployment-profiles.md) · [C-453](../stories/C-453-a-remote-approval-channel.md)

## Why

Running an agent on your own machine means your machine is what it touches. Sometimes that is exactly
right, and sometimes it is the problem: you want the *experience* local — your terminal, your approval
prompt, your model — and the *blast radius* somewhere else, wired to Docker, to sandboxed process
execution, or to microVMs in Kubernetes.

`flux tui --remote <addr>` is that: the agent you drive is here, the system it acts on is there.

This is an **additional operator-selected mode**, never a replacement for native execution. With no
target option, the runtime binds the native `System` exactly as it does today. A remote target is
explicit, immutable for the turn and unavailable to model-authored input. That is compatible with
the connector rule that a connector declares its runtime kind: the connector still decides *what
kind of effect it is*; the operator-selected system decides *where that guarded effect lands*.

The first production transport is an authenticated HTTPS daemon with WSS for long-lived byte
streams. The remote workspace is canonical in v1: there is no implicit file synchronizer, and the
surface must say that a local editor is not viewing the tree being changed unless the operator has
explicitly attached or mounted it.

## ⚠ "Remote agent" means two different things, and one of them is already shipped

This is the first thing to settle, because the two have opposite consequences and the same name.

| | **remote agent** (serve a whole agent) | **remote system** (this epic) |
|---|---|---|
| what moves | the entire agent — planning, model calls, approval | only *where effects land* |
| what stays local | a thin client | the runtime, approver, model choice and provider credentials; an approved operation-bound secret may cross |
| the analogy | the Docker **CLI**: a thin client, daemon does everything | a local process with a **remote executor** |
| status | **largely shipped** | **ships (v1 port-aware catalog)** |

The first already exists. `flux app run --serve` exposes an agent over HTTP/A2A: a
`/.well-known/agent-card.json` discovery card, `POST /a2a` JSON-RPC with `message/send` and
`message/stream`, `POST /sessions`, `GET /sessions/{id}`, `POST /sessions/{id}/messages`. If what you
want is *"someone else's machine runs the whole agent and I talk to it"*, that is the A2A surface, and
the useful work there is a **client**, not a new architecture.

⚠ The Docker analogy actually points at the *first* reading — the docker CLI is a thin client and the
daemon does everything. But *"link the runtime to the remote's system"* describes the **second**, and
the second is the more valuable one, because it keeps the property people actually care about: **you
approve on your machine, and the effect lands in a microVM.** Moving the whole agent moves the approval
prompt with it, which is the opposite of what the ask wants.

## The boundary this rides on already exists

`execution-substrate.md` states it as the epic's organizing rule:

> **`flux-runtime` decides whether something may happen. `flux-system` is where it happens.**

They are peers at L2, not stacked — and the design is explicit that fusing them *"would force every
consumer of the substrate to take flux's approval model too."* **Remote agents is that boundary put
across a network**: the deciding half stays with you, the happening half moves.

`port.rs` names the case in as many words — the port exists so a non-native substrate can serve the
same guarded operations: *"a WebAssembly embedder…, **a remote executor**, or a test double"*, and the
traits are unsealed.

So the substrate work is already filed, under [execution-substrate](execution-substrate.md):

- **[C-399](../stories/C-399-remote-guarded-io-backend.md)** — the shipped remote implementation of
  the guarded-IO port. It deliberately defines a Rust `Delegate`, not a production wire.
- **[C-397](../stories/C-397-container-process-backend.md)** — the container process backend.
- **[C-435](../stories/C-435-a-guarded-network-port.md)** — the guarded network port now provides
  bounded outbound streams plus authenticated/loopback inbound TCP and UDP resources. Migrating
  older adapter-owned listeners remains follow-up work.
- **[C-398]** — what binding `flux-system` without `flux-runtime` means; the guarantees question.

Inspection after C-399 found two additional prerequisites, now shipped. The production execution
environment carries an object-safe `ExecutionSystem` selection ([C-474]), and native process/socket
handles were replaced at the port by opaque guarded resources ([C-473]). C-475 supplies the
versioned HTTPS/WSS product protocol; C-476 owns the unsafe retry window rather than hiding it inside
the TUI story. Remote mode deliberately hides operations that have not crossed this port yet, so an
unsupported integration cannot silently execute on the local host.

⚠ **This epic is not a second copy of that work.** It is the *product* on top: the CLI surface, the
guarantees statement a user can act on, the workspace question, and the trust model. If a story here
starts re-specifying a port, it belongs in execution-substrate instead.

## Approach

### C-436 — `flux tui --remote <addr>`

The surface. What connects, what the TUI shows about *where* it is acting, and what happens when the
link drops mid-turn. ⚠ The status line must make remoteness unmissable: an operator who forgets which
machine they are on approves the wrong thing.

### C-437 — which guarantees travel, and which do not

The honest statement, and the story most likely to be skipped. flux's invariants — one egress guard,
redaction, default-deny authorization, the OS sandbox at the single spawn choke point — are stated for
the *native* substrate. Over a remote one, **some travel, some become the remote's problem, and some
simply do not apply.** C-398 already owns that question for `flux-system` in general; this is its
remote instance, and it must produce something a user can act on, not a paragraph.

### C-438 — where do the files live

⚠ **The hard one, and the one that decides whether this is usable for coding.** A coding agent's loop is
read a file, edit it, run the tests. If the runtime is local and the system is remote, then either the
files are remote (so every read crosses the network, and your editor is looking at something else) or
they are local (and you have a synchronisation problem, which is where this kind of tool usually dies).
There is no third answer that is free. C-395 made the workspace-confined file surface a port, so the
mechanism exists; the *semantics* are this story.

### C-440 — the topologies page (public docs)

⚠ **Useful now, not after the epic lands.** Most topologies already exist and nothing collects them, so
users discover the product's shape by accident. A topology is decided by where four things sit — and
they move **independently**, which is what makes this confusing without a page: the **runtime**
(decides), the **system** (does), the **model**, and the **workspace**.

The rows, with today's honest status: **fully local** (ships) · **local, OS-sandboxed** (ships;
unattended runs default to it since C-410) · **local runtime, containerized ops** (C-397, backlog) ·
**local runtime, remote system** (this epic) · **served agent + thin client** (server side ships via
`flux app run --serve`; the client is the gap) · **embedded SDK** (ships) · **portable wasm** (C-268) ·
**hosted / multi-tenant** (flux-exchange). ⚠ Every row must carry its status, and `ssh` must be named as
a legitimate option — a page that hides the free alternative to make the product look necessary is not
credible about anything else on it.

### C-453 — the approval stage, over a network

**Shipped.** `flux app run --serve --remote-approval` parks each guarded effect at `GET /approvals`
and waits for `POST /approvals/{id}`. Implemented as `flux_runtime::RemoteApprover` — one more
implementation of the existing `Approver` contract, not a second approval concept — plus the
`ApprovalGate` the server mounts those two routes over.

⚠ **State what shipped before, because an operator is running it right now.** The envelope is
**authorization → approval → guarded IO**, and approval is the only one of the three with a *human*
in it. Which posture that stage runs under is a real choice, and both answers are defensible:

| posture | who decides an effect | when it is the right answer |
|---|---|---|
| **unattended** (`--yes`) | nobody is asked; authorization policy, the fail-closed sandbox floor this surface is pinned to (C-410), and resource budgets constrain instead | high-autonomy work — research, security hardening, long exploration — where stopping at every effect is a broken agent, not a careful one |
| **remote approval** (`--remote-approval`, C-453) | a human, over the network, per effect | anything whose effects you would want to see before they land |
| **refuse** (`DenyApprover`) | nothing outside what was pre-authorised runs | a program surface with no operator attached |

The hole was not that flux shipped the wrong posture. It was that **a served agent could not choose
one.** Every approver in the tree was local — `StdinApprover` (a terminal), the TUI's
`ChannelApprover` (an *in-process* channel), `SubAgentApprover` (headless policy) — and `grep` for
approval across `flux-server` and `flux-a2a` returned nothing. So the served surface offered
`AllowApprover` or `DenyApprover`, and since the no-flag form refused to boot, **an operator serving
an agent today has been running the unattended posture** — reasonably, but not because they weighed
it against an alternative that existed.

⚠ And "refuse everything" was never quite that. C-440 traced two paths around it on the *program*
form: `assemble_integrations` spawns every installed plugin binary at startup, before any journey
exists and without consulting an approver; and a program declaring no capability policy dispatches
under `LEGACY_JOURNEY_ALLOW`, whose pre-authorised ops resolve to `Allow` and never reach an approver
either. That is why `flux app run` is pinned to the sandbox floor in its own right (C-410), and it
is unchanged by C-453 — the remote approver governs the effects that reach the approval stage, which
is not all of them on the program form.

**The four things it has to get right, and how each is held:**

1. **Silence denies.** Every non-answer — timeout, no transport, a disconnected transport, a
   cancelled turn — resolves to `Deny`. A channel that allowed on silence would be *worse* than
   `AllowApprover`, because it would look like a control. Pinned by
   `an_effect_nobody_answers_is_refused`.
2. **An approval is bound to the effect it was granted for.** A decision must echo the request's
   `fingerprint`, which is the canonical form of the effect **itself, not a digest of it** —
   including the full structured intent targets and exact plan requirements, not merely the two
   risk booleans. There is no collision to hunt for, and a `yes` shown for `write → notes.txt`
   cannot be delivered against `write → credentials.txt`. Pinned by
   `an_approval_cannot_be_delivered_against_a_different_effect`. This is also why `request_plan` is
   overridden rather than inherited: the trait default renders a whole plan as `N op(s) · summary`,
   so two unrelated plans sharing a count and a summary would share a fingerprint.
3. **Single use.** Answering removes the request; a replay finds nothing. Pinned by
   `a_replayed_decision_is_refused`.
4. **The operator boundary is structural.** The routes sit inside the server's auth layer, and an
   unauthenticated non-loopback bind is still refused at router construction. The shipped posture
   supports one shared operator token (or open loopback). It deliberately refuses principal auth:
   one deployment-wide queue would otherwise let Alice list and answer Bob's effects despite their
   session realms being isolated. A distinct supervisor authorization model is required before that
   topology can be enabled. Pinned by `answering_an_approval_requires_authentication` and
   `principal_auth_cannot_share_a_global_remote_approval_queue`.

**Deliberately not built:** a remote `AllowAlways`. Standing grants are not wrong — that is what the
unattended posture *is* — but accumulating one click by click is a posture nobody chose. And there
is no "wait forever" timeout: an unbounded wait is not a denial, it is a wedged turn.

**The road not taken.** Anthropic's Managed Agents solve the same problem by not having per-effect
approval at all — the caller steers or interrupts. That is a coherent design and, for high-autonomy
work, often the better one; it is the same thing flux's unattended posture offers. What C-453 adds
is the *choice*, not a verdict about which choice is correct.

### C-439 — trusting a remote substrate

A remote system executes your effects and reports what happened. That is a large amount of trust, and
two failures matter: an unauthenticated or hijacked endpoint, and ⚠ **a remote that lies** — reports
success it did not achieve, or omits what it did. The evidence chain is flux's core guarantee, and a
remote link is a place it can quietly stop meaning anything.

**Shipped:** bearer-authenticated TLS and guarded HTTPS/WSS refuse an unauthenticated endpoint;
delivery outcomes distinguish refused, unserved, unreachable and accepted-with-unknown-outcome.
Dispatch evidence host-stamps the immutable selected substrate's `kind` and `remotely_reported`
classification on both the call and lifecycle records, without storing its workspace path or
endpoint. This records provenance honestly; it does not prove that a remote report is true.

## Alternatives considered

- **Build a client for the existing A2A serve surface instead.** Much less work, and genuinely the right
  answer for *"someone else runs the agent."* Rejected as *this* epic because it moves approval and the
  model off your machine, which is the property the ask is trying to keep. Worth filing separately — it
  is a real gap and the server side already exists.
- **SSH into the remote and run flux there.** Free, works today, and honestly correct for many people.
  ⚠ Worth stating in the docs rather than pretending it is not an option: the epic must be better than
  `ssh`, and "better" means the local approval prompt, the local model choice, and one UI over several
  remotes.
- **Ship only the container backend (C-397) and call it done.** Covers the common case with no network
  boundary. Rejected as the whole answer: microVMs in Kubernetes are the case the ask actually names,
  and they are not reachable by a local container runtime.
- **Make the remote a flux-exchange.** Overlaps, and the exchange is the right home for *tenanted,
  credential-holding* execution. Rejected as the required path, per the charter line this repo already
  enforces: **flux must never require flux-exchange.**

## Risks & open questions

- ⚠ **The guarantees statement is the deliverable most likely to be quietly downgraded.** "Mostly the
  same" is not a statement; a table of what travels is.
- ⚠ **The files question can sink it** — see C-438. Decide it before building the link, not after.
- ⚠ **Latency.** Every op crosses the network. An agent that runs 200 file reads a turn is a different
  animal over a link than in-process; measure before promising.
- **Failure modes must stay distinguishable** — C-399's own acceptance: *"a refused operation and an
  unreachable delegate must not collapse into one error, since an operator responds to them in opposite
  ways."* Over a network that stops being a nicety.
- **Settled for v1:** model calls stay local; one immutable `--remote` target is selected per
  session; physical sandboxing and egress enforcement live on the remote execution host and are
  reported in its handshake.

## Acceptance / done

- `flux tui --remote <addr>` runs an agent locally whose effects land on the remote substrate, with
  approval, model choice and credentials staying local.
- A user can read, in one place, exactly which of flux's guarantees hold over a remote link and which
  become the remote's responsibility.
- The workspace question is answered, not deferred, and the answer is stated where a user will hit it.
- Refused, unserved, unreachable and accepted-with-unknown-outcome are structurally distinguishable;
  remote reports are labeled as reports rather than local observations.
- Nothing here makes a remote — or flux-exchange — required for local use.
