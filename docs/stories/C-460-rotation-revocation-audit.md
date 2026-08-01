---
id: C-460
title: "Rotating a secret means a restart, and nothing records which secret was used where"
pillar: Core
status: ready
priority: 7
design: docs/designs/secrets-the-agent-never-sees.md
epic: secrets-the-agent-never-sees
areas: [flux-app, flux-evidence]
note: "the operational half. Vaults re-resolves credentials periodically so `rotation, archival, or deletion propagates to running sessions without a restart`, and archiving `purges the secret; records are retained for auditing`. flux resolves once at load and keeps no record"
---

# A secret you cannot revoke without stopping the agent

## Goal

A rotated or revoked secret reaches a run in progress, and the evidence chain can answer *which secret
was used where*.

## The two gaps

**1. ⚠ Revocation is structurally impossible today, not merely absent.** The `Redactor`'s store is an
`Arc<Mutex<Vec<String>>>` with **no removal or clear API** (`crates/flux-secret/src/lib.rs:250`) —
registration is monotonic for the process lifetime. And resolution happens once. `crates/flux-app/src/secrets.rs`'s `resolve_in` runs at config load and
substitutes plaintext. Nothing re-reads it. So rotating a credential means restarting the agent, and
**revoking one has no effect at all on a run already in flight** — the value it resolved is in memory.

Vaults: *"Credentials are re-resolved periodically, both during a session and during the vault
lifecycle. This ensures that credential rotation, archival, or deletion propagates to running sessions
without a restart."*

**2. Audit exists for one hop and nowhere else.** ⚠ **Corrected after a survey** — flux *does* record
one case: `EventKind::CrossPluginResolve { consumer, provider, reference_location }`
(`crates/flux-events/src/kind.rs:128-132`, appended by `EventStoreCrossPluginAudit`) records which
consumer resolved which provider's credential **by location, never by value** (D-27). That is the right
shape and the right precedent.

What has no record: `secret "NAME"` resolution, a `$secret` header/query use in `http.request`, a
plugin's `secret`-capability read, and `env/KEY` seeding. The only output on those paths is a
`warning:` when the redactor **declines** a value for being too short
(`crates/flux-cli/src/execution.rs:757-765`). So after an incident, "was that key used, and where"
is answerable for exactly one hop.

Vaults distinguishes archive from delete for exactly this: *"Secrets are purged; records are retained
for auditing."*

## Acceptance

- [ ] **Failing-first**: a test rotating a secret mid-run and asserting the new value is used without a
      restart — failing at the merge base.
- [ ] ⚠ **Revocation is the half that matters more and is easier to skip.** Rotation is a convenience;
      revocation is what you reach for during an incident, and a revocation that does not reach a
      running agent is not a revocation. Test it separately from rotation.
- [ ] An audit record of secret *use* — ⚠ **by reference, never by value**, and it must state what it
      does not cover. A record naming which secret ran which op is useful; one that implies completeness
      it does not have is worse than none.
- [ ] ⚠ Re-resolution must not widen the trust window: a value re-read into memory is a value present
      for longer. Say what the lifetime is.
- [ ] Interacts with [C-458](C-458-substitute-at-egress.md): if a secret is a placeholder until egress,
      rotation is nearly free — the splice reads the current value. ⚠ If C-458 lands first this story
      gets much smaller; say so rather than building the hard version twice.
- [ ] Full gate green.

## Notes

- Operational rather than architectural, which is why it is priority 7 behind C-458/C-459 — but it is
  the story an operator asks for first after an incident.
- flux has no notion of credential *expiry* either. Vaults carries `expires_at` and a refresh flow for
  OAuth. Out of scope here; worth recording that flux does not model it.

## Progress
- Filed 2026-08-02 from the Vaults comparison.
