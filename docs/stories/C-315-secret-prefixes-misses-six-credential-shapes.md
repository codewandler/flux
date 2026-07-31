---
id: C-315
title: "The redactor misses six credential shapes, and two of them are the ones an agent writes production config with"
pillar: Core
status: in-progress
priority: 6
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

## Acceptance

- [ ] **Decide the shape of the fix and write the reasoning down**, because widening a prefix list is
      not obviously right. A longer list is a longer list — it does not become complete, and each
      addition trades a false-negative for a possible false-positive over ordinary prose. Entropy or
      structural detection is the alternative and has its own costs. State which and why.
- [ ] **Failing-first**: each shape this story claims to close is red before the change, using
      C-216's corpus rather than a new fixture written alongside the fix. That corpus already asserts
      these six as *uncaught* in both directions — so closing any of them **must** update the design
      doc's recorded table in the same commit, and C-216's test will red until it does. That coupling
      is deliberate; do not weaken it to make the diff smaller.
- [ ] `add_secret`'s sub-6-character no-op either becomes an error the caller can see, or is
      documented at the call site as a deliberate floor with the reason. Silent is not an option for a
      call whose whole purpose is registering something that must not leak.
- [ ] The anti-censorship guard still holds: C-216's `preserved` cases must stay preserved. A
      redactor that catches everything is not a fix, and the corpus is built to catch that.
- [ ] Full gate green in both workspaces.

## Notes

- Measured by [C-216](C-216-harness-transcript-redaction-corpus.md); the table lives in
  `docs/designs/harness-history.md` § "What C-216 measured".
- The exposure is not limited to harness history. `Redactor` is the shared redaction path, so any
  surface that redacts model-visible output inherits these gaps — harness history is just where they
  were finally measured.
- Worth reading before starting: C-216's report records that two of its *own* expectations were wrong
  on first run, and that it corrected the expectations to match observed behaviour rather than
  reshaping the fixture. Same discipline applies here.
