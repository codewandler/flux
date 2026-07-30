---
id: C-256
title: "Pin fleet A2A egress to guard-vetted addresses and re-authorize redirects"
pillar: Core
status: done
priority: 1
epic: adversarial-review-remediation-2026-07-30
design: docs/designs/adversarial-review-remediation-2026-07-30.md
areas: [flux-orchestrate, flux-a2a, flux-system]
note: "HIGH — fleet guards one DNS answer, discards it, then A2aClient re-resolves and follows redirects"
---

# Pin fleet A2A egress to guard-vetted addresses and re-authorize redirects

## Goal

Make fleet dispatch/status/cancel unable to cross the scoped private-network boundary through DNS
rebinding or an unguarded redirect.

## Acceptance

- [x] Failing-first tests use an injected resolver/redirect target to prove the current fleet path
      can validate a public answer and connect elsewhere, then prove the fixed path cannot.
- [x] Every fleet request connects only to addresses returned by `guard_url_scoped_pinned`; an empty
      vetted set fails closed.
- [x] Pinned fleet transports ignore ambient proxy variables so an unvetted proxy cannot replace
      the admitted peer or resolve the destination behind the guard.
- [x] Automatic redirects are disabled. Any supported redirect is bounded, method-safe, and every
      destination is independently re-guarded and pinned; credentials never cross origins.
- [x] `A2aClient` cannot be accidentally constructed in an unguarded mode by the fleet adapter.
- [x] Targeted A2A/orchestration tests and the standard gate are green.

## Progress

- Added `A2aClient::new_pinned`: empty address sets fail closed, redirects are disabled, and the
  client is origin-locked so agent-card/RPC adoption cannot escape its vetted host.
- Fleet and `A2aSpawner` now share one guarded constructor that must consume the resolver's vetted
  socket set. Injected-resolver tests prove the connection does not request a rebinding answer.
- A redirect-target regression test proves automatic A2A redirects reach no destination.
- The final closure review found that reqwest's ambient proxy support could still replace the
  pinned peer. The guarded client now disables proxies, with an isolated-process regression proving
  the vetted listener is reached and the configured proxy receives no connection.
- Full tests and `clippy -D warnings` pass for `flux-system`, `flux-a2a`, `flux-orchestrate`,
  `flux-credentials`, and `flux-plugin`; the integrated workspace build/test/Clippy/format gate and
  `flux-codegate` pass.

## Notes

- Evidence: primary review finding 1; `crates/flux-orchestrate/src/fleet.rs` and
  `crates/flux-a2a/src/client.rs`.
- Follow-up to C-77: native web pinning is correct; this outer adapter did not consume it.
