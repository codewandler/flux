---
id: C-291
title: "`channel webhook` — capture the raw body and verify a declared signature before parsing"
pillar: Core
status: backlog
epic: verified-webhook-channel
note: "the webhook channel authenticates with an optional STATIC BEARER TOKEN and has no signature path at all, so a vendor that signs its payloads and cannot send a custom Authorization header has no authenticated route into flux — and `Json<Value>` is an extractor, so there is no point in `handle` where the raw bytes still exist"
---

# `channel webhook` — capture the raw body and verify a declared signature before parsing

## Goal

Give a signed vendor webhook (GitHub, Stripe, Slack, Zendesk) an authenticated path into flux, by
letting a `channel webhook` declaration carry a signature-verification scheme that runs **over the raw
request bytes, before they are decoded**.

This is the foundation of the seam. C-292 … C-295 all layer on the raw-body capture this story adds
and none of them can start before it.

## Context — verified against this tree

- `WebhookSettings` is `{ addr, path, async, token }` — `crates/flux-channels/src/config.rs:18-32`.
  The only check in the request path is `constant_time_eq` against that static bearer,
  `crates/flux-channels/src/adapters/webhook.rs:88-97`. `grep -ni "hmac\|signature\|sha256"` over
  `crates/flux-channels/src/` returns nothing.
- **`Json(body): Json<Value>` (`crates/flux-channels/src/adapters/webhook.rs:86`) is an axum
  extractor** — it consumes and deserializes the body *before the handler body runs*. So verification
  cannot be inserted into `handle`; changing that signature is the structural change this story is.
- A side effect worth noticing: the existing bearer check at `webhook.rs:88-97` therefore already runs
  **after** the body has been parsed. Moving to `Bytes` puts both checks ahead of the decode.
- The declaration already parses with **no language change**. `lower_channel` puts every non-`kind`
  attribute into the settings bag (`crates/flux-lang/src/cst_decode.rs:1598-1612`), a settings value
  may be a record literal (`:2120` → `:2171`), and `secret "NAME"` is recognised at every nesting
  depth because `parse_setting_prefix` recurses (`:2096`, the `secret` case at `:2127-2130`).
- The secret therefore needs **no new machinery**: `resolve_secrets` walks `program.channels`
  (`crates/flux-app/src/secrets.rs:24-26`), recurses through objects (`:47-58`), calls
  `redactor.add_secret(resolved.clone())` at `:43` and only then substitutes the value; a missing env
  var is a hard error (`:38-42`). `build_channels` then refuses any surviving marker
  (`crates/flux-channels/src/adapters/mod.rs:23-32`, refused at `:39-45`, test at `:75`).
- No new third-party dependency: `hmac = "0.13"`, `sha2 = "0.11"`, `base64 = "0.23"`, `hex = "0.4"`
  are already `[workspace.dependencies]` (`Cargo.toml:150-153`) and flux-providers already computes
  HMAC-SHA256 with them (`crates/flux-providers/src/bedrock.rs:32`, `:42`). flux-channels is L6
  (`crates/flux-codegate/src/lib.rs:53-54`), so layering raises no question.

## Acceptance

- [ ] `WebhookSettings` gains an optional nested `verify` record: `scheme`, `algorithm`, `encoding`,
      `header`, optional `prefix`, a `signed` template over `{body}`/`{timestamp}`, an optional
      `timestamp` selector, optional `tolerance`, and `secret` written as a **host-resolved reference**
      (`secret: secret "GITHUB_WEBHOOK_SECRET"`), never a literal.
- [ ] **Failing-first test `verify_uses_raw_body_not_reserialized`.** A body whose JSON keys are
      reordered by a parse/re-serialize round trip fails verification, proving the raw bytes are what
      is checked. Any normalize-then-verify path is a bypass, not a convenience.
- [ ] **Failing-first test `bad_signature_delivers_nothing`** asserts the recording deliverer's
      delivery count is **`0`** — not merely that the response was `401`. A status assertion passes
      against a handler that rejects *and* delivers, which is the defect worth testing for.
      `crates/flux-channels/tests/e2e.rs:16-28` already has the `Tee`-over-`AppDeliverer` pattern.
- [ ] **Verification precedes the `async` branch**, not the delivery inside it. `webhook.rs:99-108`
      returns `202 Accepted` and spawns; a failure discovered after the 202 can neither report itself
      nor stop the delivery it scheduled. Test `bad_signature_delivers_nothing_in_async_mode`.
- [ ] The ordering is `headers → raw bytes → timestamp (header) → compare → tolerance → decode`, and
      the `serde_json::from_slice` is textually after the comparison in one function.
- [ ] What `Json<Value>` did implicitly is reproduced explicitly and **after** verification: a
      content-type rejection emitted before the signature check is a probe oracle. Malformed JSON is
      still a `400`.
- [ ] `verify` and `token` compose: if both are declared, **both** must pass.
- [ ] **Every failure mode returns one fixed response body**, matching the existing `"unauthorized"`
      literal at `webhook.rs:95`. Not "signature mismatch" vs "stale timestamp" vs "missing header" —
      a caller that can tell those apart has a probe for how far its forgery got.
- [ ] **Load errors, not per-request failures** — a channel that cannot honour its own declaration
      must not bind a port, consistent with an unknown channel kind being a hard error
      (`crates/flux-channels/src/adapters/mod.rs:63`):
      - an unknown `scheme`;
      - a `signed` template interpolating `{timestamp}` with no `tolerance` (see C-292);
      - a `timestamp` selector sourced from the **body** — honouring it would require parsing before
        verifying, so it is unimplementable by construction rather than merely unimplemented;
      - a resolved secret shorter than `Redactor`'s 6-character floor (see below).
- [ ] **The secret cannot surface.** Three specific requirements, each with a verified cause:
      - `Redactor::add_secret` silently drops values under 6 characters
        (`crates/flux-secret/src/lib.rs:195-201`, floor at `:198`), so a short secret is registered
        nowhere and redacted never — refuse it at load. No vendor issues a 5-character signing key,
        and one that short is cryptographically weak anyway.
      - `WebhookSettings` derives `Debug` (`crates/flux-channels/src/config.rs:18`) and already holds
        the resolved plaintext `token` at `:31`. Nothing formats it today, but a `verify` record placed
        there inherits a derive that prints the HMAC key the first time someone adds a trace line.
        Hand-write a redacting `Debug` — `OAuthToken`'s at `crates/flux-credentials/src/lib.rs:98-102`
        is the precedent, printing `"<redacted>"` while keeping `Some(_)`/`None` observable.
      - No diagnostic is built from the secret, the presented signature, or the computed digest. The
        two paths in this file that go around the redactor are the shape to avoid:
        `webhook.rs:104` (`eprintln!`) and `webhook.rs:118` (error text into the HTTP response body).
        A computed digest is a function of the secret and an attacker-supplied body — printing it is an
        oracle, not a diagnostic.
- [ ] **`verify "none"` is a distinct, deliberate declaration** — absent and explicitly-none must not
      normalise to the same thing. A webhook channel bound to a **non-loopback** address must state one
      or the other, mirroring the existing rule that a non-loopback bind requires a `token`
      (`webhook.rs:40-46`): the host auto-approves tools, so an open endpoint with no stated
      verification decision is a remote-trigger surface. ⚠ This is breaking for any existing
      non-loopback webhook program — weigh it, and if it is rejected, say so here and keep the
      tri-state at the *declaration* level anyway (C-295 needs it visible to a flow).

## Progress

- (not started)

## Notes

- **Naming: this is webhook signature verification, never "the inbound auth seam".** flux already has
  a `request-auth-seam` (`docs/designs/request-auth-seam.md`, D-64/D-68, both `done`) covering inbound
  *bearer → principal* resolution. A story titled "inbound auth" reads as a duplicate of shipped work.
- Design (in the consumer repository that motivates this):
  `../flux-connectors/docs/designs/verified-webhook-seam.md` §1, §2, §4, and
  `../flux-connectors/docs/designs/inbound-events.md` for why the vendor matrix is one parameterized
  algorithm. Upstream source of the parameters is flux-connectors' `HmacSpec`
  (`crates/connector-spec/src/inbound.rs`), generated per vendor from the vendor's own documentation.
- **Rejected alternative — a tower middleware layer.** It also sits ahead of the handler, but a layer
  that wants the raw body must buffer the stream itself and hand the copy downstream, putting two
  places in the channel that materialise the body. That is the seam a normalize-then-verify bypass
  grows along. The channel has exactly one route (`webhook.rs:71`), so an in-handler check is
  sufficient, un-bypassable, and keeps one buffering site.
- **Rejected alternative — verify in the `Deliverer`.** One implementation for every transport, but
  the `Deliverer` receives an already-parsed `Value` (`webhook.rs:110`), so it is downstream of the
  parse by construction — the wrong side of the line this story draws.
- **Adjacent, worth a decision here:** `WebhookChannel::router` (`webhook.rs:63-73`) attaches no
  `DefaultBodyLimit`, unlike flux-server which applies an explicit one over its whole surface as C-189
  (`crates/flux-server/src/lib.rs:913`, `:1120`). After this story the per-request cost for an
  *unauthenticated* caller includes an HMAC over the whole body, so that limit becomes a security
  parameter rather than a hygiene default.
