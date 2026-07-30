---
id: C-293
title: "Webhook challenge/handshake — answer endpoint verification without waking an agent"
pillar: Core
status: backlog
epic: verified-webhook-channel
note: "Slack's url_verification echo and Meta's hub.challenge GET arrive at the same path as real events; waking a journey to answer a handshake is both wasteful and a way to hand vendor-shaped text to an agent for no reason — and the channel routes only `post` today, so the GET half needs a route"
---

# Webhook challenge/handshake — answer endpoint verification without waking an agent

## Goal

Let a `channel webhook` satisfy a vendor's endpoint-verification handshake itself, so registering a
webhook does not depend on an agent happening to echo the right field back.

## Context — verified against this tree

- The channel registers exactly one route, and it is `post` only:
  `Router::new().route(&self.path, post(handle))` — `crates/flux-channels/src/adapters/webhook.rs:71`.
  Meta-style `hub.challenge` arrives as a **GET** with query parameters, so it does not reach the
  handler at all today.
- Every request that does reach `handle` goes to `deliver` (`webhook.rs:110` sync, `:103` async),
  which wakes a journey. There is no path that answers a request without a delivery.

## Acceptance

- [ ] An optional `challenge` declaration on the channel: which field or query parameter carries the
      token, and what to echo. The two shapes to cover are Slack (`type == "url_verification"` in the
      body → echo the `challenge` field) and Meta (GET with `hub.challenge` → echo it).
- [ ] **Failing-first test `challenge_answers_without_delivery`**: the handshake receives the correct
      response body **and the recording deliverer's delivery count is `0`** — no trigger fires, no
      journey runs, no model call happens.
- [ ] The channel answers a **GET** on its path when a `challenge` declares one, and continues to
      reject GET otherwise. A GET route that exists unconditionally is a new unauthenticated surface
      on every webhook channel, so gate it on the declaration.
- [ ] **The challenge path is subject to the same verification as an event where the vendor signs
      it**, and where the vendor does not, that is stated explicitly in the declaration and in the docs
      rather than left implicit. An unsigned challenge that is answered is a small oracle for endpoint
      existence — acceptable, but only deliberately.
- [ ] A challenge-shaped body that does not match the declaration is treated as an **ordinary event**,
      not silently swallowed. Test `unmatched_challenge_shape_is_delivered_normally`.
- [ ] The echoed value is returned verbatim in the shape the vendor expects, and nothing else about
      the request is reflected. Echoing an attacker-chosen string is the point of the handshake; make
      sure it is the only thing echoed.

## Progress

- (not started)

## Notes

- Depends on **C-291** (raw-body capture and the `verify` declaration). Independent of C-292 — a
  handshake can be implemented and tested before the scheme matrix lands.
- Design: `../flux-connectors/docs/designs/verified-webhook-seam.md` §5, capability 5.
- This is the one capability in the seam that is not about verification. It is filed with the seam
  because it shares the request path and the "answer without a turn" property, and because a vendor
  will not deliver a single real event until its handshake is answered — so it is on the critical path
  for the first live inbound integration whether or not it is about signatures.
