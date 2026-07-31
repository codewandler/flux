---
id: C-315
title: "The redactor misses six credential shapes, and two of them are the ones an agent writes production config with"
pillar: Core
status: done
areas: [flux-secret]
note: "measured by C-216's corpus, not guessed — SECRET_PREFIXES has `sk-` but not Stripe's `sk_live_`, and PEM private-key bodies are unprefixed base64; below 6 characters `add_secret` silently no-ops, so short credentials have no recourse at all"
---

# The redactor misses six credential shapes

## Goal

`flux-secret`'s `Redactor` catches credentials by prefix. C-216 built a corpus over real transcript
shapes and **measured** what that misses, pinning the result in both directions (caught shapes stay
caught, uncaught shapes stay uncaught, so neither can drift silently). Six shapes are not caught:

1. **AWS secret access key** (`wJalr…`, 40 chars) — no prefix at all. Only the access-key *id*
   (`AKIA…`) has one, so the half that authenticates is the half that leaks.
2. **A password inside a connection URL** (`postgres://user:pw@host`) — `:` is a token boundary, so
   the password is its own unprefixed token.
3. **Stripe secret key `sk_live_…`** — the list carries `sk-`, with a hyphen. A one-character
   mismatch between the OpenAI spelling and the Stripe spelling.
4. **Hugging Face `hf_…`** — not on the list.
5. **GitLab `glpat-…`** — not on the list.
6. **PEM private-key material** — the `-----BEGIN…` delimiters are prose and the body is unprefixed
   base64.

3 and 6 are the sharp ones, and they co-occur: an agent writing a production config is exactly where
a Stripe live key and a private key appear together.

**The recourse and its floor.** `add_secret` catches all six when the operator registers the value —
C-216 asserts this. But below **6 characters** `add_secret` silently no-ops, so a short credential
has no recourse except leaving the datasource off entirely. A silent no-op on a security-registration
call is its own defect independent of the prefix list.

## The decision

**Not a longer prefix list, and not entropy. Four mechanisms, three of which read *context* rather
than the token itself.**

The six shapes do not share a defect, so they cannot share a fix. Three of them (`sk_live_`, `hf_`,
`glpat-`) really are missing prefixes and cost one line each. The other three are not prefix
problems at all: an AWS *secret* access key is 40 characters of base64 with nothing distinguishing
it, a connection-URL password is bounded by `:` and `@` rather than by a marker, and PEM key
material is unprefixed base64 framed by prose. No addition to `SECRET_PREFIXES` reaches any of them.

**Entropy scoring is the only mechanism that would**, and it was rejected. It cannot distinguish 40
characters of AWS secret from a git SHA, a checksum, a UUID, a base64 PNG or a minified asset — and
C-216's corpus contains a base64 PNG and a unified diff *specifically* as `preserved` cases, because
a redactor that catches everything is not a fix. `Redactor` is the shared redaction path for the
stream-json writer, the whatif cassette, the approval sheet, the evidence flush and harness ingest,
so one false positive destroys information on all of them at once. The chosen mechanisms fail toward
false *negatives*, which is the right direction when the alternative is censoring the operator's own
diff back at them.

So:

1. **Prefixes gain per-prefix length floors** rather than the flat `>= 8`. This is what makes adding
   `hf_` — three characters, and the stem of every Hugging Face identifier in existence — safe at
   all: its floor is 30, just under a real 37-character token. `sk_test_` was deliberately *not*
   added; a Stripe test key is not production material and C-216 did not measure it, and an
   unmeasured addition is exactly the drift this table exists to resist.
2. **A PEM private-key block pass.** Body to one `[redacted]` line, delimiters kept — they are not
   secret, and they are the only thing that makes the redaction legible. Scoped to `PRIVATE KEY`, so
   certificates and public keys are untouched. An unterminated block redacts to the end of input,
   because a key truncated by `flux-system`'s output byte cap is the *common* shape, not an exotic
   one.
3. **A URL-credential pass.** Structural rather than heuristic: userinfo containing a colon is a
   credential by the URL grammar, so the false-positive rate is zero by construction. Only the
   password goes.
4. **A contextual assignment rule.** `NAME=VALUE` where the name declares a secret and the value
   looks like opaque material. This is the only rule that redacts a token carrying no marking of its
   own, so both halves are deliberately narrow — the guard rails and the classes of false positive
   each one buys back are enumerated in the design doc.

**What this is not.** It is not complete, and the residual gaps are measured rather than asserted
away: a value under the 16-character opaque floor, a bare token with nothing naming it, the
`key: value` spelling, and an all-digit credential. Each is in the corpus, in `UNCAUGHT`, failing
the test the moment it silently becomes caught.

**The side effect worth naming.** With the prefix list emptied, C-216's mutation test used to see all
six corpus shapes leak; it now sees three. Every credential in an env-dump or heredoc *line* is also
named by its own assignment, so the contextual rule is genuine defence in depth — and that change in
sensitivity is recorded in the expected map rather than glossed.

## Acceptance

- [x] **Decide the shape of the fix and write the reasoning down**, because widening a prefix list is
      not obviously right. A longer list is a longer list — it does not become complete, and each
      addition trades a false-negative for a possible false-positive over ordinary prose. Entropy or
      structural detection is the alternative and has its own costs. State which and why.
- [x] **Failing-first**: each shape this story claims to close is red before the change, using
      C-216's corpus rather than a new fixture written alongside the fix. That corpus already asserts
      these six as *uncaught* in both directions — so closing any of them **must** update the design
      doc's recorded table in the same commit, and C-216's test will red until it does. That coupling
      is deliberate; do not weaken it to make the diff smaller.
- [x] `add_secret`'s sub-6-character no-op either becomes an error the caller can see, or is
      documented at the call site as a deliberate floor with the reason. Silent is not an option for a
      call whose whole purpose is registering something that must not leak.
- [x] The anti-censorship guard still holds: C-216's `preserved` cases must stay preserved. A
      redactor that catches everything is not a fix, and the corpus is built to catch that.
- [x] Full gate green in both workspaces.

## Progress

Implemented on `impl/C-315`.

- `crates/flux-secret/src/lib.rs` — the four passes, `try_add_secret`/`Unregistered`/
  `MIN_REGISTERED_SECRET_LEN`, and 9 new unit tests. Crate bumped `1.0.1 → 1.1.0` (additive; the
  protocol-line rule and `check-crate-versions.sh` require a bump on any content change).
- `crates/flux-capabilities/tests/harness_redaction_corpus.rs` — the six shapes moved
  `under_match → redacted`; four residual gaps added to the fixture and to `UNCAUGHT`; the weakening
  model extended to mirror all four mechanisms, with two new weakenings (`structural passes
  removed`, `assignment rule removed`) and their exact expected sensitivity.
- Call sites onto the fallible form: `flux-runtime`'s `seed_redactor` now **returns** the refs it
  could not register; `flux-app`'s `resolve_secrets` and `flux-web`'s `$secret` path refuse rather
  than proceed unprotected; `flux-cli`'s plugin `SecretSink` and provider-env seeding warn.

Left deliberately, and reported to the coordinator rather than fixed here: `flux-web`'s
`redact_json` skips `Value::Number`, which is a hole in the "registration is total" guarantee the
all-digit shape depends on. Measured in the corpus, recorded in the design doc, not fixed — it is
another story's blast radius.

## Notes

- Measured by [C-216](C-216-harness-transcript-redaction-corpus.md); the table lives in
  `docs/designs/harness-history.md` § "What C-216 measured".
- The exposure is not limited to harness history. `Redactor` is the shared redaction path, so any
  surface that redacts model-visible output inherits these gaps — harness history is just where they
  were finally measured.
- Worth reading before starting: C-216's report records that two of its *own* expectations were wrong
  on first run, and that it corrected the expectations to match observed behaviour rather than
  reshaping the fixture. Same discipline applies here.
