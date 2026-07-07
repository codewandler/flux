---
id: A-49
title: AgentCard conformance fields — protocolVersion, honest interfaces/preferredTransport, optional metadata
pillar: Agent
status: ready
priority: 3
epic: a2a-conformance
design: docs/designs/a2a-conformance.md
note: "Tier-1 quick-win: the card omits protocolVersion (spec-required) and emits interfaces: [] though it serves a JSON-RPC endpoint"
---

# AgentCard conformance fields

## Goal
Make flux's discovery card conformant with the A2A v1.0 AgentCard schema by emitting the
spec-required `protocolVersion` and declaring the transport interface flux actually serves, plus the
low-cost optional metadata clients expect. No behavior change to auth or the RPC surface — this is
card-shape only.

## Why (evidence)
- `crates/flux-a2a/src/types.rs:417-453` — `AgentCard` has **no `protocolVersion` field** (the A2A
  spec makes it required) and **no `preferredTransport`**; `interfaces: Vec<AgentInterface>`
  (`types.rs:438-439`) is modeled but the builder emits it **empty** (`server.rs` `agent_card`,
  `interfaces: Vec::new()`), so a card advertises no transport even though it serves JSON-RPC at `url`.
- Absent optional fields clients read: `provider`, `documentationUrl`, `iconUrl`,
  `supportsAuthenticatedExtendedCard`.

## Acceptance
- [ ] `AgentCard` gains `protocol_version` (serde `protocolVersion`), `preferred_transport`
      (`preferredTransport`), and optional `provider`/`documentation_url`/`iconUrl`/
      `supports_authenticated_extended_card` — all `skip_serializing_if` so existing cards that don't
      set them stay byte-stable, except `protocolVersion` which is always emitted (spec-required).
- [ ] `server::agent_card` + `flux-server::a2a::build_agent_card` (`crates/flux-server/src/a2a.rs:186-231`)
      populate `protocolVersion` (the spec version flux targets), a single `interfaces` entry for the
      JSON-RPC endpoint (`{ transport: "JSONRPC", url: <the card url> }`), and `preferredTransport:
      "JSONRPC"`. `provider`/`documentationUrl`/`iconUrl` are populated from `CardInfo`/config when set.
- [ ] `supportsAuthenticatedExtendedCard: false` (honest — no extended-card method yet; the method
      itself is a Tier-2/3 follow-up, tracked in the epic).
- [ ] Failing-first test: an card-shape assertion (extend the existing card tests in
      `crates/flux-server/tests/` or `flux-a2a`) that the served card carries `protocolVersion`, a
      non-empty `interfaces` whose JSON-RPC entry url equals the card `url`, and `preferredTransport`.
- [ ] `AgentCard::rpc_endpoint` (`types.rs:458-472`) still resolves correctly against the now-populated
      `interfaces` (regression guard — it already prefers `url`, but the interfaces path is now live).
- [ ] Docs: the AgentCard row(s) in the support matrix flip to ✅.

## Progress
- (not started)

## Notes
- Additive/non-breaking: new fields serialize only when present; `protocolVersion` is the one always-on
  addition and is required for conformance.
- Keep `capabilities.pushNotifications: false` (honest) — do not advertise unsupported capabilities.
- Related: A-50 (error codes), A-52 (the extended-card method is a natural sibling once auth-gated card
  content exists). Epic: [a2a-conformance](../designs/a2a-conformance.md).
