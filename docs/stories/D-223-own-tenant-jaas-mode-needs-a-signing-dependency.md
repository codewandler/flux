---
id: D-223
title: "Own-tenant JaaS mode needs an RS256 signing dependency — a decision, not an implementation"
pillar: Agent
status: blocked
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels]
note: "split out of D-206, which delivered the guest-token path and reported this half BLOCKED. Nothing in the workspace can sign RS256 — no `rsa`, no `jsonwebtoken`, no `ring`; `sha2` alone is not enough. Adding one is a genuine third-party crypto dependency and is the user's call, which is why it is a story rather than a fence lift"
---

# Own-tenant JaaS mode, and the signing dependency it needs

## Goal

Let flux mint its **own** JaaS JWT from a tenant API key, instead of borrowing a guest token — and
first, get a decision on the crypto dependency that requires.

[D-206](D-206-jaas-brave-talk-room-backend.md) landed the guest-token path: `JaasTokens` is a trait,
`JaasRoom` refreshes under a live session, and the MUC JID is taken from the conference response.
Own-tenant mode is a **second implementation of that same trait** — the shape is already there. What
is missing is the ability to sign.

⚠ **This is blocked on a decision, not on effort.** The workspace has no RS256 signer:

- no `rsa`, no `jsonwebtoken`, no `ring` in `Cargo.toml`;
- `sha2` is present but signing needs more than a hash;
- D-206's guest path needed no crypto at all — claim *decoding* is `base64` + `serde_json`, both
  already dependencies.

So this is a new third-party crypto dependency in a security-relevant position, which is the user's
call. That is why D-206's coordinator lifted the fence for `reqwest` (already a workspace dependency,
five crates deep) and explicitly did **not** lift it for this.

## Acceptance

- [ ] **The dependency decision is made and recorded** — which crate, why it over the alternatives,
      and what its audit/maintenance story is. Record it in the design doc, not just the commit.
      A crypto dependency in the credential path deserves a written reason.
- [ ] `JaasTokens` gains an own-tenant implementation that signs an RS256 JWT from a JaaS API key
      (`kid`, tenant, room, expiry, and the moderator/participant claims JaaS expects).
- [ ] The API key is read through the **credential seam**, never a literal, and is registered with
      the `Redactor`. A signing key is the highest-value secret in this path.
- [ ] **Failing-first**: a test that mints a token and verifies it against the public half, failing
      at the merge base because no signer exists.
- [ ] Tests do not touch live Brave/8x8 endpoints — same constraint D-206 held to.
- [ ] Full gate green.

## Notes

- The trait boundary D-206 left is the whole integration surface: `JaasTokens::guest_token` takes a
  room name and returns a token, and never sees a URL. An own-tenant impl slots in beside it.
- ⚠ D-206's acceptable-use constraint carries over: this is for a tenant the operator controls. An
  own-tenant signer makes room *creation* trivially scriptable in a way the guest path does not, so
  whatever this lands should not become an enumeration or bulk-join surface. D-206 recorded that
  constraint in the module's "Acceptable use" section — keep it true.
- Vendor wire mechanics in this area were captured from a 2026-07-30 spike and have not been
  re-verified against the live API since. Treat them as that fresh.

## Progress

- Filed 2026-08-01, split out of D-206 at its BLOCKED report so the guest-token path could ship
  without waiting on a dependency decision.
