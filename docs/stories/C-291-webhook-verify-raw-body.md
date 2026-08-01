---
id: C-291
title: "`channel webhook` — capture the raw body and verify a declared signature before parsing"
pillar: Core
status: done
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

- [x] `WebhookSettings` gains an optional nested `verify` record: `scheme`, `algorithm`, `encoding`,
      `header`, optional `prefix`, a `signed` template over `{body}`/`{timestamp}`, an optional
      `timestamp` selector, optional `tolerance`, and `secret` written as a **host-resolved reference**
      (`secret: secret "GITHUB_WEBHOOK_SECRET"`), never a literal.
      → `crates/flux-channels/src/config.rs` (`VerifyDecl` / `VerifySpec` / `VerifySelector`);
      lowering proven end-to-end by `tests/webhook.rs::a_verify_record_is_writable_in_a_program`,
      which asserts the record reaches `ChannelDecl.settings` with `secret` as a `{"$secret":…}`
      marker. ⚠ "never a literal" is *documented, not enforced*: `resolve_secrets` substitutes the
      marker before these settings deserialize, so at this layer a resolved reference and a literal
      are the same string. See the Progress note.
- [x] **Failing-first test `verify_uses_raw_body_not_reserialized`.** A body whose JSON keys are
      reordered by a parse/re-serialize round trip fails verification, proving the raw bytes are what
      is checked. Any normalize-then-verify path is a bypass, not a convenience.
      → `adapters/webhook.rs` unit tests; the body carries reordered keys, a duplicate key **and**
      non-canonical whitespace, and the round trip is asserted to change it before anything else runs.
- [x] **Failing-first test `bad_signature_delivers_nothing`** asserts the recording deliverer's
      delivery count is **`0`** — not merely that the response was `401`.
      → `adapters/webhook.rs` unit tests, over a `Counting` deliverer. Every negative case in this
      story asserts the count.
- [x] **Verification precedes the `async` branch**, not the delivery inside it.
      → all three guards sit above `if state.is_async` in `handle`; test
      `bad_signature_delivers_nothing_in_async_mode` sleeps past the `202` before asserting `0`.
- [x] The ordering is `headers → raw bytes → timestamp (header) → compare → tolerance → decode`, and
      the `serde_json::from_slice` is textually after the comparison in one function.
      → `handle` is `(State, HeaderMap, Bytes)` and its five numbered steps end in `from_slice`.
      The `timestamp` / `compare` / `tolerance` sub-steps live inside `SignatureVerifier::verify`,
      which is C-292's half of the seam; what this story fixes is that they are *reachable* over raw
      bytes at all, and that the decode is downstream of them.
- [x] What `Json<Value>` did implicitly is reproduced explicitly and **after** verification: a
      content-type rejection emitted before the signature check is a probe oracle. Malformed JSON is
      still a `400`.
      → `json_content_type` at step 4, `from_slice` → `400` at step 5;
      `every_authentication_failure_returns_one_fixed_body` asserts both are unreachable before
      authentication and both still fire after it.
- [x] `verify` and `token` compose: if both are declared, **both** must pass.
      → `a_token_and_a_verify_scheme_must_both_pass`, all four combinations.
- [x] **Every failure mode returns one fixed response body**, matching the existing `"unauthorized"`
      literal. → `UNAUTHORIZED_BODY`, one `unauthorized()` helper, asserted on the response *body*
      across four probes.
- [x] **Load errors, not per-request failures** — a channel that cannot honour its own declaration
      must not bind a port:
      - an unknown `scheme` → `each_defect_in_a_verify_record_reports_itself`;
      - a `signed` template interpolating `{timestamp}` with no `tolerance` →
        `a_timestamped_template_without_a_usable_tolerance_is_a_load_error` (an unparseable
        `tolerance` too);
      - a `timestamp` selector sourced from the **body** → `a_body_sourced_timestamp_is_a_load_error`;
      - a resolved secret shorter than `Redactor`'s 6-character floor →
        `a_signing_secret_too_short_to_redact_is_refused_and_never_echoed`.
      All of them run in `from_decl`, which `build_channels` calls before any listener —
      `the_refusal_happens_in_build_channels_before_anything_binds` pins that.
- [x] **The secret cannot surface.** Three specific requirements, each with a verified cause:
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
- [x] **`verify "none"` is a distinct, deliberate declaration** — absent and explicitly-none must not
      normalise to the same thing. A webhook channel bound to a **non-loopback** address must state one
      or the other. ⚠ Breaking — **weighed and taken**; see the Progress note.
      → `Verification::{Unstated, None, Scheme}` with `is_stated()`;
      `a_non_loopback_bind_must_state_a_verification_decision` and
      `an_absent_verification_and_a_stated_none_are_different_facts`.

## Progress

**Done (C-291). The seam is in; C-292 plugs into `verifier_for`.**

- **The structural change.** `handle` is now `(State, HeaderMap, Bytes)`. `Json<Value>` was an
  extractor, so the decode ran before the handler body — before even the *bearer* check. The order is
  now `open-channel guard → bearer → signature → content-type → decode`, all in one function, with
  `serde_json::from_slice` textually last.
- **Where this story ends and C-292 begins.** `SignatureVerifier` (raw `&[u8]` in, one `bool` out) is
  the whole interface. `verifier_for(&VerifySpec) -> Option<Arc<dyn SignatureVerifier>>` is the single
  plug-in point; it returns `None` today and `build_verification` turns that `None` into a **load
  error**, after every structural rule about the declaration has already been checked. No crypto
  dependency was added — computing a digest is C-292's, and a manifest change this story was fenced
  from.
- **⚠ Breaking, deliberately.** A non-loopback `channel webhook` with a `token` and no `verify` used
  to load and now does not. Weighed and taken: the fix is one line of program text, the error prints
  it verbatim, the epic is the road to *stable* (so the break costs least now), and the alternative is
  that the decision stays permanently unmade for every public webhook flux has ever run. The
  *loopback* case is untouched — silence there is a local endpoint, not an exposure.
- **The public-bind rule is now keyed on the property, not on `token.is_none()`** (C-321's lesson):
  `is_effectively_open(token, verify)`. That is what lets a signature-verified channel face the
  network with **no bearer at all**, which is the Goal — a vendor that signs its payloads and cannot
  send a custom `Authorization` header now has an authenticated route in.
- **Two independent refusal sites, proved independently.** Deleting the load-site guard reds
  `non_loopback_requires_token` + `a_verifying_channel_needs_no_bearer_token_to_face_the_network`
  while the whole unit suite stays green; deleting the handler-site guard reds
  `an_effectively_open_non_loopback_channel_refuses_every_request` while the whole integration suite
  stays green. Six further guards were mutation-tested the same way (missing-verifier refusal,
  signature check, raw-body capture, auth-before-decode ordering, the redactor floor, the redacting
  `Debug`).
- **Not enforced, and it cannot be here: `secret` "never a literal".** `flux_app::resolve_secrets`
  substitutes the `{"$secret":…}` marker before `WebhookSettings` deserializes, so by `from_decl` a
  host-resolved reference and a plaintext literal are the same `String`. What *is* enforced is every
  consequence that matters — the 6-character redactor floor, an empty secret, and a hand-written
  `Debug` that prints `"<redacted>"`. Enforcing the spelling itself needs a marker that survives
  resolution (a decl-level flag), which is a flux-lang change and its own story.
- **`MIN_SECRET_LEN` is a restated `6`, not an import.** flux-channels does not depend on
  `flux-secret` and adding it is a manifest change this story was fenced from, so the constant carries
  a doc comment naming `flux_secret::MIN_REGISTERED_SECRET_LEN` instead of a compile-time link. A
  one-line dev-dependency would let a test pin the two together.
- **Adjacent, still open:** `WebhookChannel::router` attaches no `DefaultBodyLimit` (the story's own
  closing note). After this change an *unauthenticated* caller's per-request cost will include an HMAC
  over the whole body once C-292 lands, so that limit becomes a security parameter rather than a
  hygiene default. Deliberately not fixed here — it is a separate decision with its own default to
  choose.

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
