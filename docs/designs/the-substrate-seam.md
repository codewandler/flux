# Design — The substrate seam: what crosses it, and who may compose it

**Status:** planning · **Pillar:** Core · **Stories:** C-689 (DNS), C-690 (clock offset),
C-691 (WebSocket out), C-692 (WebSocket in), C-693 (callers on a shared substrate) · builds on
[first-class-hosts.md](first-class-hosts.md), [host-metrics-seam.md](host-metrics-seam.md),
[execution-substrate.md](execution-substrate.md)

## The rule this epic applies

`ExecutionSystem` is an **authorization boundary, not a virtualization layer**. Something belongs
on it when three things hold: it is a real effect with a blast radius, it can be honestly refused
(a fail-closed `Unserved` means something), and the answer genuinely differs by substrate. That
rule is why the seven delivered families are there, and it is what decides the candidates.

- **DNS passes all three.** Resolution is how a destination becomes an address, it can be refused,
  and a pod's view of a name is genuinely not the laptop's. It is currently a *test* seam
  (`HostResolver` in `net.rs`) that never reaches the port, so the coordinator resolves names the
  substrate will dial — C-689.
- **Clock passes only the third, weakly.** No blast radius, nothing to refuse. It is substrate
  *condition*, so it belongs in the metric vocabulary that already carries `uptime` — C-690.
- **WebSocket passes all three** in both directions: outbound is on the port but native-only and
  absent from the wire (C-691); inbound is guarded at `bind_tcp` and the TCP framing layer, but the
  accepted session — where the per-message ceilings live — is unmodelled (C-692).
- **Caller identity is not a port family at all**; it is the question of who the port answers for,
  which the two axes answer oppositely today — C-693.

## Where authority actually lives

Three independent ceilings decide what a deployed agent can do, and the effective set is their
intersection:

1. **Coordinator-side authorization** — what the agent may ask for: tool policy, permission
   subjects, approval posture, capability ceilings. Per-principal on the agent surface, which has
   `ServerAuth::Principal` with realm-scoped sessions and per-request `(Caller, Trust)`.
2. **The host grant** — whether this surface class may select the binding at all. Decision 0018
   rule 4, deny-by-default, per surface class rather than per user (C-678 adds the visibility half).
3. **Substrate capability** — what the far side can do at all: the serving process's OS identity,
   its pinned workspace, its posture, its network policy, and the guarded port's fail-closed
   defaults.

The gap is that layer 3 has no per-caller dimension: the remote-system wire carries one bearer
token and no principal. So agent-side realm scoping constrains what the *agent* does, never what
the *substrate* permits. Until C-693 settles it, the operating rule is **one substrate per
authority level** — which is already the shape of the Kubernetes profile's one-writer-per-workspace
deployment.

## Overlays: which families are separable

Composing a substrate from per-family backends — "network from the host, filesystem from the pod" —
is mechanically expressible today, and one instance already ships: C-675 attaches a `GuardedHttp`
implementation from L5 to a system whose other families are native. So the question is not whether
it works but **which compositions are coherent**, and the families do not divide evenly.

- **The execution locus** — process, workspace files, host files, env — is one machine's reality.
  A process spawned in a pod writing to a workspace on the laptop is not a security posture, it is
  an incoherence: the workspace a process sees *is* the file family. These must not be split.
- **Egress** — dial, HTTP, and DNS with them — is genuinely separable, and separating it is an
  ordinary operational pattern (a proxy, an egress gateway, `docker run --network=host`).
- **Descriptive** — metrics and identity — must follow the locus and must never be forged; a
  composite that reports one substrate's identity for another's effects breaks the audit record.

So the useful shape is not an arbitrary overlay matrix but **one execution locus with selectable
egress**. Two things must be settled before it ships, and both are decision-shaped rather than
story-shaped: `SubstrateIdentity` is a single answer per system and would have to name the
family→backend mapping instead; and composing two separately granted bindings can produce an
authority neither grant intended — a sandboxed locus with host egress is exactly the hole the
sandbox was meant to close. That decision belongs in the roadmap repository beside 0018.

## Endpoints and datasources across the boundary

A host says *where* an effect happens. An endpoint says *what service, and where its credential
lives*. A datasource is the governed read over such a service. The three compose, and the
composition is missing exactly one thing at each joint.

Discovery itself is already brokered and pluggable — `EndpointBroker` fans an `endpoint.discover`
query across provider plugins, the kubernetes provider already returns Services, Ingresses and
crossplane/RDS-derived database endpoints as weak references with `kubernetes/<ns>/<secret>/<key>`
credential locations, and `flux endpoint import --from-json` persists one. So discover → import →
use is a closed loop today. What the loop drops is **locality**.

- **`EndpointRef` records no host.** `postgres://db.default.svc.cluster.local:5432` is meaningless
  on a laptop and exactly right inside the cluster, and the record cannot tell them apart — so the
  guard resolves the name on the wrong machine, the private-network grant that admits it is
  caller-wide rather than the binding's, and an imported record is no more useful than a typed
  URL. **C-709.**
- **Discovery has no vantage.** A query answers "what can be discovered", implicitly from wherever
  the provider ran. A host binding *is* a vantage point, and scoping a query to one makes "what
  does my dev cluster see" askable and its answer attributable. **C-715.**
- **A datasource has no locality.** `LiveDatasource` receives a `ToolContext` that carries the
  selected substrate and is required to do its IO through guarded surfaces, so the machinery
  exists — but nothing ties the connection to the substrate its endpoint is reachable from.
  **C-716.**

Together they make one sentence true: the host is where the connection is made from, the endpoint
is what is connected to, and the grant is who may. Each of the three is small on its own and none
of them is coherent without the others.

## Candidate families not yet filed

Verified absent from the tree, listed so the next reader does not re-derive them: a **PTY /
interactive process** family (nothing in the workspace allocates one — this is what an
interactive shell or a TUI *inside* a host would need), **filesystem watch** (no inotify/notify
anywhere), **process control beyond spawn** (signal/kill exists only on a test double),
**substrate OS identity** (uid/gid/groups — `SubstrateIdentity` carries kind, workspace,
confinement and provenance, but not who the daemon runs as, which is precisely the ceiling in the
authority discussion above), **unix-domain sockets**, and **device access** (decision 0018 rule 6
already anticipates backend-specific capacity). Each would have to pass the three-part rule on its
own merits.
