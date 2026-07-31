---
id: C-313
title: "The fifth encoder copy, and the query key nothing pins"
pillar: Core
status: ready
priority: 14
areas: [flux-web]
note: "filed from C-303's review, not from planning — the shared RFC 3986 encoder landed and two crates were converted, but flux-plugin kept a byte-identical private copy while the design doc claims the tree has one encoder; separately, no test observes that the query KEY is encoded"
---

# The fifth encoder copy, and the query key nothing pins

## Goal

C-303 introduced a shared RFC 3986 percent-encoder (`crates/flux-core/src/urlencode.rs`) and
converted `flux-credentials` and `flux-providers` onto it. Two loose ends survived that review, both
non-blocking and both recorded here rather than fixed in the merge.

**One.** `docs/designs/web-capabilities.md` states that the tree has "one encoder for the whole
tree". That sentence is false as committed: `crates/flux-plugin/src/host.rs:1861` still holds a
byte-identical private RFC-3986 copy. C-303's own Acceptance — "not a fifth private copy" — is
genuinely met, because the *new* encoder is shared; what is wrong is the design doc's claim about
the rest of the tree. C-303's Progress note says `flux-plugin/src/host.rs` was fenced off for that
run, which if accurate makes the surviving copy correct and the sentence the only defect.

**Two.** No test observes that the query **key** is percent-encoded. C-303's reviewer probed it:
changing `out.push_str(&encoded_key)` to `out.push_str(key)` in `append_query`
(`crates/flux-web/src/http.rs`) leaves all 72 `codewandler-flux-web` tests green. The code is
correct; the line is simply unpinned. This is not a C-303 Acceptance item — item 1 says "each
*value*" — but it is the same injection class the story exists to close, and an unpinned line is how
that class comes back.

## Acceptance

- [ ] **Failing-first test**: a test that fails when the query key is passed through unencoded and
      passes when it is encoded. Prove it by making exactly the mutation above and showing the test
      name in the failure output.
- [ ] Decide, and state, whether `flux-plugin`'s private copy is consolidated onto
      `flux_core::urlencode` or deliberately kept. Either is a legitimate answer — the layering rule
      or the fence may make a dependency edge wrong — but the decision must be written down.
- [ ] `docs/designs/web-capabilities.md` tells the truth about how many encoders the tree has,
      whatever the answer to the previous item turns out to be.
- [ ] Grep for any *further* private percent-encoders and account for each one. The reviewer found
      the `flux-plugin` copy by reading, not by a check — so if more exist, this is the story that
      should have found them.
- [ ] Full gate green in both workspaces.

## Notes

- Filed from the C-303 review (2026-07-31). Neither finding blocked that merge; the reviewer's
  verdict was PASS.
- Related: [C-303](C-303-http-request-structured-query.md) introduced the shared encoder.
- Worth noting for whoever picks this up: the shared encoder deliberately emits **upper-case** hex,
  because SigV4 requires it — `flux-providers`' delegation is only semantically identical to the
  loop it replaced under that spelling. Don't "normalize" it to lower case.
- The distinction from L-101's form encoder is real and tested
  (`a_space_is_pct_twenty_not_plus`) — a space is `%20` here, not `+`. Any consolidation must not
  collapse the two.
