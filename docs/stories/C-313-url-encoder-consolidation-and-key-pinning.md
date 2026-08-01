---
id: C-313
title: "The fifth encoder copy, and the query key nothing pins"
pillar: Core
epic: road-to-stable
status: in-progress
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

- [x] **Failing-first test**: a test that fails when the query key is passed through unencoded and
      passes when it is encoded. Prove it by making exactly the mutation above and showing the test
      name in the failure output.
      → `query_key_is_percent_encoded_like_its_value` (`crates/flux-web/src/http.rs`, `mod tests`).
      Mutation proof, not a merge-base proof — see Progress.
- [x] Decide, and state, whether `flux-plugin`'s private copy is consolidated onto
      `flux_core::urlencode` or deliberately kept. Either is a legitimate answer — the layering rule
      or the fence may make a dependency edge wrong — but the decision must be written down.
      → **Consolidated.** `expand_endpoint_template` now calls
      `flux_core::percent_encode_component`; the private copy is deleted. Reasoning in Progress.
- [x] `docs/designs/web-capabilities.md` tells the truth about how many encoders the tree has,
      whatever the answer to the previous item turns out to be.
      → the "one encoder for the whole tree" sentence is replaced by a scoped claim plus its two
      qualifications (the form encoder; the twelve copies in `plugins/`).
- [x] Grep for any *further* private percent-encoders and account for each one. The reviewer found
      the `flux-plugin` copy by reading, not by a check — so if more exist, this is the story that
      should have found them.
      → full census in Progress; **thirteen** further copies existed, twelve of them in `plugins/`,
      one of which has already drifted.
- [x] Full gate green in both workspaces.
      → root: build, `--workspace` tests, `clippy -D warnings`, `fmt`, `flux-codegate`.
      `plugins/`: `cargo fmt --check` clean; see Progress for why nothing more is owed there.

## Progress

Landed on `impl/C-313`. Two independent loose ends, and **neither is a behaviour change** — so the
shape of the evidence differs from a normal story and is stated here rather than left to inference.

**The key pin is a mutation proof, and a merge-base proof would have been dishonest.** The code was
already correct at the merge base, so a test written against the base *passes* — that proves the
behaviour exists, not that the test observes the line. The artifact that means something is the
reviewer's own probe, re-run: with `out.push_str(&encoded_key)` changed to `out.push_str(key)` in
`append_query`, `query_key_is_percent_encoded_like_its_value` fails on
`GET /s?q&injected=1=cats HTTP/1.1` while **all 80 other tests in the crate stay green** — which is
precisely the finding (the line was load-bearing and unobserved). Restored, 81 pass. The test uses a
key of `q&injected=1` rather than one carrying a space, so the failure is the smuggled parameter
itself and not a URL-parse error on the way; the space and non-ASCII byte classes are covered in the
same test by a direct `append_query` assertion, where no URL parser is involved.

**The `flux-plugin` copy is consolidated, and C-303's Progress note checks out.** That note says the
copy "was left in place only because `crates/flux-plugin/src/host.rs` was fenced off for this run —
converting it is a two-line follow-up and the natural place is whoever next touches that file". It
is accurate, and the three things that could have made converging wrong are all clear:

- **No manifest edit.** `crates/flux-plugin/Cargo.toml` already carries `dep:flux-core` under the
  `host` feature, `host` is in `default`, and the whole `host` module is `#[cfg(feature = "host")]`
  — so `flux_core` is in scope wherever the copy compiled at all. `host.rs` already used
  `flux_core::` paths (e.g. the `Error::Http` mapping). The dependency-list fence is not touched.
- **No layering problem.** `flux-plugin` is L4, `flux-core` is L0; the edge already exists and
  `workspace_respects_layering` stays green.
- **No behaviour change.** The two functions were semantically byte-identical (the only textual
  difference was match-arm ordering, `-_.~` vs `-._~`). This is why no failing-first test is offered
  for it: there is no behaviour to fail. What the change buys is *observation* — the call site moves
  from a copy with **zero** tests to `flux_core::percent_encode_component`, which has four.

**The census — every percent-encoder in the repo, accounted for.** The reviewer found the
`flux-plugin` copy by reading; this is the systematic sweep the story asked for.

| where | dialect | status |
|---|---|---|
| `flux-core::percent_encode_component` | RFC 3986 | **the shared one** |
| `flux-web` `append_query` | RFC 3986 | delegates |
| `flux-credentials` `urlencode` | RFC 3986 | delegates (C-303) |
| `flux-providers` `percent_encode_segment` | RFC 3986 | delegates (C-303) |
| `flux-plugin` `percent_encode_component` | RFC 3986 | **converged by this story** |
| `flux-lang` `urlencode_component` | form (`+`) | deliberately separate — L-101 |
| `plugins/{alertmanager,confluence,docker,gitlab,grafana,huggingface,jira,loki,opsgenie,prometheus,slack,websearch}` | RFC 3986 | **twelve copies, cannot delegate today** |

After this story the **root workspace has one RFC 3986 encoder**. The twelve in `plugins/` are a
real remaining exposure and cannot be closed from here: `plugins/` is a workspace excluded from the
root, nothing in it depends on `flux-core`, and `host-kit` — the crate every plugin already links —
exposes no encoder, only `join_url`. **One of the twelve has already drifted:**
`plugins/gitlab/src/operations/mod.rs:20` omits `~` from the unreserved set, so it emits `%7E` where
the other eleven emit `~`. That is the copy-drift the design doc warns about, observed rather than
predicted. Closing it means adding the shared spelling to `host-kit`, which is a
plugin-protocol-line change (C-143) and owes a pack release — hence its own story, not this one.
Filed as a finding for the coordinator rather than fixed here.

**What is still unobserved, deliberately.** `expand_endpoint_template` has no test of any kind — the
encoding it applies to a config value substituted into an `EndpointSpec::template` is unpinned
exactly the way the query key was. Pinning it needs a fake `PluginSystem` (`GuardedProcess` +
`GuardedHostFiles` + `GuardedEnv`), which no test in `host.rs` has today; `resolve_config` reads env
through `self.system().env(key)` rather than `std::env`, so such a fake is feasible and would not
need unsafe env mutation. That is a fixture-sized piece of work outside this story's Acceptance.

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
