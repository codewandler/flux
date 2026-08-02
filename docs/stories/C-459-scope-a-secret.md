---
id: C-459
title: "A secret has no scope — once resolved it can go anywhere the egress guard already allows"
pillar: Core
status: ready
priority: 5
design: docs/designs/secrets-the-agent-never-sees.md
epic: secrets-the-agent-never-sees
areas: [flux-secret, flux-app, flux-policy]
note: "⚠ CORRECTED after a survey: flux DOES scope which secret may be NAMED (http.request's allowed_secrets, C-76; plugin grants.secrets), it does not scope WHERE a named secret may GO. Two different axes, and only the first exists. Per-principal scoping is newly expressible after C-408/C-415"
---

# Which destinations, and on whose behalf

## Goal

A secret carries its own scope: **which destinations** it may be sent to, and **which principal** may
cause it to be used.

## The two gaps

**1. Destination.** ⚠ **flux already scopes one axis and not the other, and the distinction is the
story.**

*What exists* — flux scopes **which secret may be named**:
- `http.request`'s `$secret` **allowlist** (`crates/flux-web/src/http.rs:50`, enforced in
  `resolve_secret_env` at `:407-413` **before the value is read**), from `[web] allowed_secrets` or
  `FLUX_WEB_SECRET_ALLOW`. C-76's point exactly: a prompt-injected model cannot name
  `AWS_SECRET_ACCESS_KEY` and exfiltrate it in one call.
- Plugin manifest grants: `grants.secrets` is a per-plugin **key list** and `grants.credential` a
  per-plugin boolean, both deny-by-default.
- Cross-plugin use is gated by an operator config grant per `(consumer, provider)` pair, then optional
  first-use approval, then audit — **before** materialization (`broker.rs:665-702`).

*What does not exist* — flux scopes nothing about **where a named secret may go**. `guard_url_scoped`
decides whether *this caller* may reach *this host*; it knows nothing about which secret is in the
request. Once `resolve_in` has substituted plaintext into the settings bag
(`crates/flux-app/src/secrets.rs:52`), that value is an ordinary string usable by anything holding
those settings, bound for any host the caller may already reach.

⚠ Also dangling: `Sensitivity` (`crates/flux-secret/src/lib.rs:124`) is **defined and read by nothing**.
Either it becomes the carrier for this scope or it should go.

Vaults scopes per credential — `networking.allowed_hosts`, described as preventing *"your key from ever
being shared with unauthorized hosts"* — and pairs it with `injection_location` (header, body, or both)
on the reasoning that *"request payloads are often assembled from content the agent is working with, so
the request body is the broader exposure surface."*

**2. Principal.** A vault is *"the collection of credentials associated with an end user"*, referenced
per session. flux has no equivalent — and ⚠ **it newly could**: [C-408](C-408-room-participants-share-one-identity.md)
and [C-415](C-415-a-room-triggered-journey-still-runs-as-the-operator.md) established per-speaker
`TurnIdentity`, so "which principal may use this secret" is expressible where it was not before. On a
shared surface — a room with several humans in it — that is the difference between a credential the
operator holds and a credential anyone in the room can spend.

## Acceptance

- [ ] **Failing-first**: a test asserting a destination-scoped secret is refused for an out-of-scope host
      that the caller is otherwise permitted to reach — failing at the merge base.
- [ ] Destination scope is **default-deny** where declared, and the check happens on the **resolved,
      vetted** address, matching the discipline `guard_target_host_pinned` already enforces. ⚠ A scope
      matched against the pre-resolution hostname is a bypass.
- [ ] ⚠ **A secret with no declared scope keeps working.** Breaking every existing `secret "NAME"` to add
      scoping would guarantee nobody adopts it. Unscoped must remain valid and must be *visible* as
      unscoped.
- [ ] Principal scope, built on the existing `TurnIdentity` — ⚠ **not a second identity concept.**
      C-415's lesson holds: one constructor, one trust decision.
- [ ] Injection location (header / body) decided — implement it, or record why flux's shape does not
      need it.
- [ ] Full gate green.

## Notes

- ⚠ Worth doing **independently of [C-458](C-458-substitute-at-egress.md)**: scoping applies to every
  secret whatever the transport, while substitution only fits HTTP-shaped egress. If only one of the two
  ships, this is the one with broader reach.
- Related from the other side: [D-227](D-227-outbound-a-call-is-an-effect-that-costs-money.md)'s
  destination allowlist for outbound calls is the same idea for a different resource — check whether one
  mechanism serves both before building two.

## Progress
- Filed 2026-08-02 from the Vaults comparison.
- 2026-08-02 — implementor reported COMPLETE on `impl/C-459`; gate green in its worktree including the
  `FLUX_BWRAP_BIN=/nonexistent/bwrap` posture. ⚠ **HELD UNMERGED, awaiting an independent review.** The
  adversarial reviewer commissioned for it died on the org's monthly spend limit, and so did the C-453
  implementor — no agent can be spawned.
  The coordinator read it single-handed and confirmed four of the flagged surfaces: the `NAME;to=host`
  parser **fails closed** on an empty name, a non-`key=value` part, an empty value, an unknown key and a
  bad `in=` (`is_unscoped()` explicitly requires `unusable.is_none()`, so a malformed entry constrains
  everything rather than nothing); an unusable grant refuses at the use site and an unmatched name is
  `NotAllowlisted`; `Destination` has private fields and exactly one constructor, `vetted`, which
  refuses an empty pin set; and the guarded-path reorder keeps **one** DNS resolution with an explicit
  authority-equality assertion, the comment naming re-resolution as the TOCTOU itself.
  ⚠ **That is one context, not two, and it is the context that wrote the dispatch** — the weakest form
  of independence. The change adds a security check *and modifies the guarded egress path* C-77
  hardened, so it waits. Minor note for whoever reviews: the authority check compares
  `(scheme, host, port)` and not userinfo — unreachable via `append_query`, so harmless as written, but
  it is the kind of omission that matters if that code is ever reused.
  **Nothing about this is a finding against the diff.** It is preserved, gated and ready; it has not
  been accepted.
