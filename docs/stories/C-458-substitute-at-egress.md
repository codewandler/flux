---
id: C-458
title: "Give the agent a placeholder, not the secret — splice the real value in at the guarded send"
pillar: Core
status: ready
priority: 5
design: docs/designs/secrets-the-agent-never-sees.md
epic: secrets-the-agent-never-sees
areas: [flux-secret, flux-web, flux-app]
note: "⚠ prevention instead of containment. flux's three known secret bugs — C-339 (redaction fails OPEN), C-432 (cannot redact what it was never told), D-234 (a Debug impl prints it) — are all containment failures and NONE is expressible in a substitution model. Feasibility crux verified: flux has one egress GUARD but not one egress SENDER"
---

# The failure that authenticates wrong instead of leaking

## Goal

For at least one class of secret, the agent holds an **opaque placeholder** and the real value is
spliced in at the guarded send — so there is nothing to scrub.

## Why the strategy matters more than the mechanism

| | containment (today) | prevention (this story) |
|---|---|---|
| what fails | a scrub misses | a substitution misses |
| what happens | **a credential is published** | the third party rejects a placeholder |
| how you find out | later, from someone else | immediately, as an auth error |

⚠ Containment fails silently and in the worst direction. That is not a hypothesis here — it is flux's
record: [C-339](C-339-redaction-falls-back-to-the-unredacted-value.md) had redaction returning the
**unredacted** value when redacted text stopped parsing;
[C-432](C-432-browser-credentials-never-come-from-the-prompt.md) is the `Redactor` being unable to redact
what it was never told; [D-234](D-234-mediasettings-debug-prints-argv.md) is a `Debug` impl printing a
resolved secret. **None of the three is expressible if the value was never there.**

## ⚠ flux already does this — on the connector path only

[C-312](C-312-connector-credential-boundary.md) asserts it as an invariant: *"flux holds exactly ONE
secret on this path — the deployment session bearer. A response carrying credential-shaped material is
refused, not merely redacted."* The vendor credential never enters the process.

The **local** path does the opposite: `crates/flux-app/src/secrets.rs:35-41` — `resolve_in` reads
`std::env::var(&name)` and puts the **plaintext into the config value**, then seeds the `Redactor`.
This story brings the local path toward what the connector path already proves.

## ⚠ The feasibility crux, verified before filing

flux has one egress **guard** and **not** one egress **sender**. `crates/flux-web/src/egress.rs` is the
model-facing web family's single guarded, address-pinned sender — the natural splice point — but
`flux-providers`, `flux-a2a`, `flux-channels`' JaaS tokens and `flux-auth` each construct their own
`reqwest` client. Only **model-facing** egress needs this (a provider call is flux's own, not the
agent's), which keeps the scope tractable — but it is a prerequisite, not a detail.

## Acceptance

- [ ] **Failing-first**: a test asserting a secret-referencing config yields a placeholder in the
      agent-reachable value, and that the real value appears only in the bytes leaving the guarded
      sender — failing at the merge base.
- [ ] ⚠ **The placeholder is unforgeable and per-session.** A stable placeholder that leaks tells an
      attacker exactly which string to send to get the real one spliced in. Decide and justify.
- [ ] ⚠ **A non-substituted placeholder is sent literally, not stripped.** Anthropic's rule, and it is
      the right one: a request arriving with the literal placeholder is a *diagnosable* failure; a
      request with the field silently removed is a mystery.
- [ ] ⚠ **Scope is stated, because it cannot be universal.** flux secrets also feed XMPP MUC passwords,
      SIP registrar credentials and plugin argv, where there is no HTTP egress to splice at. Say which
      secrets are prevented and which are still merely contained — **an operator who cannot tell the
      difference has a false sense of safety**, which is worse than uniform containment.
- [ ] The `Redactor` stays as the backstop everywhere. This makes it no longer the *only* defence; it
      does not retire it.
- [ ] The clients this breaks are documented up front: anything validating the credential's format at
      startup, or signing a request with it (AWS SigV4), receives a placeholder and fails.
- [ ] Full gate green.

## Notes

- ⚠ Open, and it decides the shape: splice in `flux-system`'s guarded send (reaches more callers) or in
  the web family where the pinned client already lives (smaller, and `egress.rs` already handles
  per-redirect-hop rebuilding — note its own comment that *"even a GET body can contain credentials"*).
- Substitution is **outbound only**. If a secret is used to fetch a session token, the returned token
  arrives in the clear — Anthropic documents the same limit. Do not imply otherwise.

## Progress
- Filed 2026-08-02 from the Vaults comparison.
