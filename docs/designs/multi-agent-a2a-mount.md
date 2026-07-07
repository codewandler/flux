# Design: resolver-keyed multi-agent A2A mount

**Status:** implemented (story [D-63](../stories/D-63-multi-agent-a2a-mount.md)) · **Layer:** L6
(`flux-server`) · **Owner:** Timo

## Why

flux-server's A2A surface serves exactly **one** agent at fixed routes (`/a2a`,
`/.well-known/agent-card.json`) over one `Arc<FlowEngine>`. But it already owns the hard parts an
N-agent host needs: A2A session-TTL retention (C-18), the C-29 queued-mint ordering,
`message/stream` SSE, and (now) D-69 per-principal isolation. A multi-tenant host that wants to
serve many agents therefore rebuilds its own mount — and in doing so **forgoes** flux's session
lifecycle, running stateless turns. D-63 generalizes the surface: N agents keyed by path, each with
flux's full A2A machinery, resolved per request.

## Shape — a resolver, not a rewrite

```rust
#[async_trait]
pub trait AgentResolver: Send + Sync {
    async fn resolve(&self, agent_id: &str, auth: Option<&AuthContext>) -> Option<ResolvedAgent>;
}
pub struct ResolvedAgent { pub engine: Arc<FlowEngine>, pub card: Arc<CardInfo> }
```

`router_multi(resolver, auth)` mounts, per agent id path segment:

- `GET  /health` — liveness (shared, unprefixed)
- `GET  /:agent_id/.well-known/agent-card.json` (+ `/agent.json` alias) — discovery, **public**
- `POST /:agent_id/a2a` — JSON-RPC 2.0 (`message/send`, `message/stream`)

The single-agent [`router`] is unchanged; `StaticResolver` (a name→agent map) is the built-in for a
program that declares its agents up front, and dynamic hosts (per-tenant agents minted at runtime)
implement `AgentResolver` themselves — e.g. keyed on the authenticated principal.

### Why this reuses everything

Each agent owns its **own** `FlowEngine`, and thus its own event store. So A2A session TTL,
`contextId` continuity, and D-69 realm scoping are already isolated per agent with zero new
bookkeeping — the multi-agent handler resolves the engine from the path and calls the *same*
`send`/`subscribe`/`create_a2a_session` functions the single-agent handler does. There is no
per-agent session-map keyed by `(agent_id, session)`: the engine *is* the key.

## The answered open questions

- **Auth injection point** (the question D-63 shared with D-64): auth is **one outer layer**, not
  resolver-owned. `require_auth` runs first (all three D-69 modes), then the resolver receives the
  already-authenticated `AuthContext` and may scope which agents a principal can see — but it never
  verifies tokens. One verification point; the public discovery routes keep their structural
  exemption.
- **Card URL per mount**: the card advertises the endpoint a client must actually POST to, so its
  `url` is built with the mount prefix — `<base>/<agent_id>/a2a`. In principal mode `<base>` is the
  configured `external_url` (never the request `Host` — the public card is a token-phishing surface
  otherwise, exactly as in D-69); in open/shared-secret mode it is `Host`/`X-Forwarded-Proto`
  derived. One shared `build_agent_card(card, auth, headers, prefix)` serves both mounts.
- **Streaming pins the resolved agent**: resolution happens once per request, before dispatch; the
  spawned SSE task owns its engine clone, so an agent can never be re-resolved mid-stream (a tenancy
  hazard the design explicitly rejects).
- **Unknown agent** → the same constant 404 as any unauthorized resource (A2A §13.1), on both the
  card and the RPC route.

## Isolation & the safety envelope

Per-agent engines mean cross-agent isolation is structural: a task addressed to `/support/a2a`
runs on support's engine and lands only in support's store; sales' store never sees it. Under
principal mode, D-69's per-turn envelope identity still applies on each resolved engine — a turn
runs the safety envelope under the request principal's `(Caller, Trust)`. The single-turn gate is
shared across agents in v1 (safe: it can only over-serialize, never under-serialize an identity
swap); per-agent gating is a noted follow-up.

## Scope boundary

The mount serves the **A2A protocol surface** (card + `/a2a`) a multi-agent host needs — the exact
surface a downstream consumer hand-rolled while forgoing flux's TTL/SSE. The full per-agent REST
session surface (`/:agent_id/sessions/*`) is intentionally out of scope: the single-agent `router`
remains the way to expose the REST API for one engine, and no reviewed consumer needs the REST
surface multiplexed. If that need arrives it is an additive follow-up (the resolver already yields
the engine; only per-agent route wiring is missing).

## Testing (hermetic)

- Two agents behind a `StaticResolver`: each card is served at its own path and advertises its own
  `/:agent_id/a2a`; a task to one agent lands only in that agent's store (isolation); an unknown
  agent id is a constant 404 on both routes.
- `contextId` continuity within one agent (two sends, one session).
- Auth composes as one layer: principal mode 401s the RPC route without a token, and the public
  card declares the bearer scheme and derives its url from `external_url`.

## Non-goals / follow-ups

- Per-agent REST session routes (above).
- Per-agent turn gate (v1 shares one).
- A dynamic resolver caching layer / agent-card `protocolVersion` — orthogonal.
