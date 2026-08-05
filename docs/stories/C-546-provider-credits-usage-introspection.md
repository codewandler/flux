---
id: C-546
title: "Generalized provider credits/usage/subscription introspection, callable by the model and shown in the TUI"
pillar: Core
status: ready
priority: 34
epic:
design:
areas: [flux-provider, flux-providers, flux-tui, flux-tools]
note: "a capability-gated Provider op returns credits, usage against limits (session/weekly windows) and subscription identity; exposed as a model-callable op and a TUI surface"
---

# Generalized provider credits/usage/subscription introspection, callable by the model and shown in the TUI

## Goal

The provider layer generalizes account introspection: a capability-gated operation on the provider
abstraction (`crates/flux-provider/src/lib.rs` — `Provider` trait at :195 as of 2026-08-05)
returns available credits, current usage against limits across the provider's windows (session,
weekly, monthly — whatever that provider actually has), and the identity of the subscription/plan
being billed. The result is available in two consumers: the model can call it as an op (so an agent
can plan around its own remaining budget), and the TUI shows it (so an operator sees credit/limit
state without leaving flux).

## Acceptance

- [ ] A capability-gated introspection operation exists on the provider abstraction: providers that
      expose account/usage APIs implement it; providers that do not declare the capability absent,
      and callers get a typed "not supported" rather than an error. Failing-first tests for both a
      supporting and a non-supporting provider.
- [ ] The returned shape is provider-generic: available credits (when the provider has a credit
      balance), usage against each limit window the provider defines (named windows with reset
      times, e.g. session and weekly), and the subscription/plan identity in use. Per-provider
      adapters map their real endpoints into it; at least two concrete adapters ship (e.g.
      OpenRouter credits and one OAuth-subscription provider), each with a fixture-driven test.
- [ ] The op is model-callable: it is registered in the tool/op surface so an agent can query its
      own provider's remaining budget mid-run; a test proves the call round-trips through the op
      layer.
- [ ] The TUI exposes the same data (placement may reuse or sit beside
      [C-542](C-542-granular-time-token-budgets-visible-in-tui.md)'s budget surface); a TUI test
      proves rendering, including the "provider does not support introspection" state.
- [ ] The gate is green in both workspaces.

## Progress

- (not started)

## Notes

- Filed 2026-08-05 via /track:story. Same motivating morning as
  [C-545](C-545-do-not-retry-quota-exhausted-429.md): both the codex weekly limit and the Anthropic
  credit exhaustion were invisible until requests failed — introspection would have shown both
  before dispatching work.
- Related: [C-545](C-545-do-not-retry-quota-exhausted-429.md) classifies the failure after the
  fact; this story sees it coming. [C-542](C-542-granular-time-token-budgets-visible-in-tui.md)'s
  budget meter is the natural TUI neighbor (spend-vs-budget beside credits-vs-limit).
- Not every provider exposes usage APIs; the capability gate is the contract, not best-effort
  scraping. Where an API exists but needs a different credential scope, surface that as the typed
  unsupported reason.
