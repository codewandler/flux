---
id: C-675
title: "A selected native substrate serves HTTP"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-system, flux-web]
design: first-class-hosts
note: "C-651/C-652 interplay: a sandboxed selection answers Unserved for web effects — fail-closed, and a capability gap both implementors flagged"
---

# A selected native substrate serves HTTP

## Goal

Under a selection that resolves to a native-composed substrate — the sandboxed peer today, the
container backend next — `http.request` and `web.fetch` answer the port's `Unserved`, because a
bare `System` serves no HTTP and the one native implementation (`flux_web::NativeHttp`) lives a
layer above the substrate. That refusal is honest and fail-closed, and it is also a gap: selecting
confinement should not cost web effects. Give selected native substrates an HTTP backend without a
second client, without an ambient seam, and without the routing branch learning to sniff kinds.

## Acceptance

- [ ] A sandboxed selection serves `http.request`/`web.fetch` through the one reviewed egress
      client with its own audit sink; the codegate `Http` census still counts exactly the existing
      client construction points.
- [ ] The selection branch stays kind-blind and nothing can fall back to a local send while a
      selection is in force; the placement census is unchanged.
- [ ] A spawned sub-agent's context carries the parent's selected substrate, pinned by the test
      C-652's review named as the open question.
- [ ] `SandboxedSystem`'s `GuardedHttp` census entry moves from empty to its new truth with a
      review note stating which call is made and why it adds no IO path.

## Design

The question is a seam, not a feature: the one HTTP client is at L5 (`flux_web::NativeHttp`, over
`flux-web`'s reviewed egress broker) and the substrate that needs it is at L2 (`SandboxedSystem`
composing `System`). Dependency arrows point downward, so the substrate cannot fetch the client.
The answer is that it does not have to — something above both already holds both halves.

### The seam: attachment at the composition site

`System` gains one field, an optional `Arc<dyn GuardedHttp>` behind `port::AttachedHttp`, set only
through the builder `System::with_http`. `impl GuardedHttp for System` makes exactly one call on
that backend, and answers the port's `Unserved` when nothing was attached. `SandboxedSystem`'s
`GuardedHttp` stops being empty and becomes the same one-line delegation it already uses for
network and metrics: `GuardedHttp::http_request(&self.inner, …)`.

Nothing in `flux-system` builds a client, gains a dependency, or learns what `flux-web` is: what
travels is an implementation of **its own trait**, reached through `dyn`. The join happens once, at
L6 — `flux-cli` builds `NativeHttp` from the session's resolved web wiring (the same
`WebOptions` the ops are registered with: private-net scope, `PrivateNetAdmit` audit sink,
grant-source label) and attaches it to the system it hands to substrate selection. A substrate
composed from that system serves web effects through the same client, guard and audit sink an
unselected run uses.

### Why this shape and not the ones C-652 rejected

- **Not a second client in `flux-system`** — nothing is constructed here; the codegate `Http`
  census still counts exactly the existing `reqwest::Client` construction points.
- **Not a process-global broker** — this is a field on one value, cloned with it. There is no
  ambient registry for a later caller to consult or replace, and the security-relevant fact
  (attached or not) is a property of the substrate the operator selected.
- **Not a six-trait decorator** — no wrapper re-implements six families to change one. The peer
  already delegates every family to the system it composes; HTTP now travels the same path.

### What deliberately does not change

- **The session's own native `System` is never attached to.** The attachment rides on a clone
  handed to selection, so an unselected run — and a named `local` binding, which installs no
  override — stays on the bare native path where the family is fail-closed exactly as C-652
  shipped it. `http.request`/`web.fetch` still reach their own reviewed native backend there.
- **The routing branch is untouched and stays kind-blind.** It still asks only *whether* a
  substrate was selected. A selected substrate that serves no HTTP (a `RemoteSystem`, or a peer
  composed over an unattached system) still refuses, and nothing falls back to a local send.
- **Confinement still means what it meant.** A sandboxed peer confines what this process
  *spawns*; a request made in this process against this machine's network is the same request the
  composed `System` would make — the call `GuardedMetrics` already makes for the same reason.

### The sub-agent half

`task` snapshotted `ctx.execution_system()` onto the `SpawnRequest`, which resolves an absent
selection to the native `System` — so every child carried a selection its parent never made, and
the one family that reads the narrower question then refused the child's web effects. It now
snapshots `ctx.selected_execution_system()`: the parent's selection when there is one, `None` when
there is not.

### Placement note

Selection now resolves after the session opens, because the substrate's audit sink is a
session-scoped value (event store + session id). Every binding refusal is still a startup refusal.
