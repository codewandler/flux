---
id: C-292
title: "Webhook signature schemes — one parameterized HMAC, constant-time, replay-bounded"
pillar: Core
status: backlog
epic: verified-webhook-channel
note: "four vendors' 'unique' schemes collapse to one algorithm over {digest, encoding, signed-template, tolerance}; test vectors must come from vendor documentation, never from our own implementation — and the verifier must accept a SET of candidate digests, because Stripe sends several v1= during a secret rotation"
---

# Webhook signature schemes — one parameterized HMAC, constant-time, replay-bounded

## Goal

Implement the verification C-291 declares, covering the real vendor schemes with **one parameterized
algorithm** rather than a function per vendor — and make replay bounded, because a timestamped scheme
with no window is a signature that replays forever.

## Context — verified against this tree

GitHub, Stripe, Slack and Zendesk each document a bespoke-looking signature. They vary along exactly
four axes: which digest, how it is encoded, what string is signed, and how long a signature stays
acceptable.

| vendor | header | algorithm | encoding | signed | window |
|---|---|---|---|---|---|
| GitHub | `X-Hub-Signature-256` | sha256 | hex, `sha256=` | `{body}` | — |
| Stripe | `Stripe-Signature` | sha256 | hex, `t=`/`v1=` pairs | `{timestamp}.{body}` | tolerance |
| Slack | `X-Slack-Signature` | sha256 | hex, `v0=` | `v0:{timestamp}:{body}` | 5m |
| Zendesk | `X-Zendesk-Webhook-Signature` | sha256 | base64 | `{timestamp}{body}` | tolerance |

- `constant_time_eq` already exists in the file and is length-aware —
  `crates/flux-channels/src/adapters/webhook.rs:123-132`. Reuse it for the digest compare; flux has no
  `subtle` in its workspace and does not need one for a fixed-length comparison.
- `hmac = "0.13"`, `sha2 = "0.11"`, `base64 = "0.23"` and `hex = "0.4"` are already workspace
  dependencies (`Cargo.toml:150-153`), and `crates/flux-providers/src/bedrock.rs:32,42,697-704` is a
  working HMAC-SHA256 usage to copy the API shape from (`hmac` 0.13 moved `new_from_slice` onto the
  re-exported `KeyInit` — see the comment at `bedrock.rs:31`).

## Acceptance

- [ ] HMAC with `sha256` **and `sha1`** (GitHub's legacy `X-Hub-Signature`), `hex` and `base64`
      encodings, an optional literal prefix, and a `signed` template over `{body}` / `{timestamp}`.
- [ ] **Failing-first test `vendor_signature_vectors_verify`** using vectors taken **from each
      vendor's own documentation** — GitHub, Stripe, Slack, Zendesk. Self-generated fixtures agree
      with our implementation by construction and prove nothing.
- [ ] The comparison is **constant-time**. A `==` on the digest is the defect under test; assert on
      the code path, not on timing.
- [ ] **`tolerance` is enforced for every timestamped scheme.** Test `stale_timestamp_is_rejected`
      with a signature that is otherwise perfectly valid, and `future_timestamp_is_rejected` with one
      beyond the window in the other direction (clock skew cuts both ways, and a far-future timestamp
      is an unbounded replay ticket).
- [ ] **A `{timestamp}` template with no `tolerance` is a load error**, refused by C-291's loader. It
      is not a warning and not a default: a scheme that timestamps without a window is strictly worse
      than one that does not timestamp at all, because it reads as though replay were handled.
- [ ] **The verifier accepts a *set* of candidate digests, not one.** Stripe sends more than one `v1=`
      during a secret rotation and a verifier must accept if **any** candidate matches. A scheme
      modelled as "one header value, one digest" silently fails every request for the whole rotation
      window. Test `any_candidate_signature_matches`.
- [ ] **Negative matrix, each asserting a delivery count of `0`:** wrong secret, mutated body (one
      byte), truncated signature, missing header, wrong prefix, unknown prefix, empty signature,
      signature valid for a *different* body.
- [ ] Every rejection returns C-291's single fixed response body — the negative matrix must not be
      distinguishable from the outside.

## Progress

- (not started)

## Notes

- Depends on **C-291** (raw-body capture and the `verify` declaration). Nothing here is startable
  before it.
- Design: `../flux-connectors/docs/designs/verified-webhook-seam.md` §2 and §3;
  `../flux-connectors/docs/designs/inbound-events.md` for the matrix finding.
- **Open upstream — Stripe's composite header is not yet expressible in the connector-side spec.** The
  header value is `t=…,v1=…,v0=…`: the digest is neither the whole value nor a literal prefix of it,
  and the timestamp is a *component of that same header* rather than a header of its own. flux-connectors
  owns the declaration fix (its C-59/C-60); **flux's side is unaffected in shape** — accept a set of
  candidates and read the timestamp from a header — so this story is not blocked on the answer. Track it
  before wiring Stripe's vector.
- **The timestamp is always header-borne, by construction.** It is an input to the comparison that
  decides whether the body may be parsed at all, so reading it out of the body would invert C-291's
  whole ordering. No vendor in the matrix needs otherwise.
