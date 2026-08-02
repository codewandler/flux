---
id: C-476
title: "Remote operation delivery — idempotent identities and an honest unknown outcome"
pillar: Core
status: done
epic: remote-agents
design: docs/designs/remote-agents.md
areas: [flux-system, flux-server, flux-evidence]
note: "a link can die after a side effect and before its receipt; blindly retrying would duplicate mutations, while reporting unreachable would pretend nothing happened"
---

# Remote operation delivery

## Goal

Make a dropped remote link safe to reconcile without claiming exactly-once execution for arbitrary
effects.

## Acceptance

- [x] Every submitted operation carries a caller-minted operation id and canonical effect
      fingerprint; reusing an id for a different effect is refused.
- [x] The daemon durably records acceptance before execution and retains bounded terminal results for
      reconnect/status queries.
- [x] A reconnect with the same id returns the recorded state/result and never starts a second copy.
- [x] Mutating work is never automatically resubmitted under a new id. A daemon crash after possible
      execution reports `Unknown`, distinct from `Refused`, `Unserved`, `Unreachable` and reported
      execution failure.
- [x] Cancellation and client disconnect have explicit states and cannot silently convert to success.
- [x] Retention has byte/count/age ceilings and never persists raw secret values.
- [x] Failing-first tests cut the link before acceptance, during execution, after the effect but
      before the receipt, and across daemon restart.

## Progress

- Filed from C-436's dropped-link acceptance: C-399's failure modes describe whether an answer
  arrived, but not whether an in-flight mutation happened.
- 2026-08-02: caller ids and SHA-256 request fingerprints are checked before a bounded acceptance
  ledger is written atomically. Same-process replay returns the terminal answer, collisions refuse,
  and restart without a retained answer reports `Unknown`. Count, encoded-byte and age ceilings are
  enforced; the ledger contains no arguments, results or secret values. WSS disconnect drops its
  resource handle, killing a managed child or closing a listener/stream.

## Notes

- Depends on C-475. C-439 consumes these states in evidence provenance and operator diagnostics.
