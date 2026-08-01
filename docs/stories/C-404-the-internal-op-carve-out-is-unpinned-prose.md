---
id: C-404
title: "The credential boundary's `internal: true` carve-out is prose with no test pinning it, and `internal_op` is public"
pillar: Core
status: ready
priority: 10
epic: connector-platform
areas: [flux-plugin, flux-cli, flux-codegate]
note: "found by C-403's review. The carve-out is NOT dormant — it is the only thing excusing `plugin.validate` at crates/flux-cli/src/plugin_cmd.rs:517, a live host-dispatched site whose `problems`/`warnings` are printed to operator stdout and whose error frame is printed raw. host-kit's own docs describe an internal op returning credentials, so the benign census is a property of today's plugins, not of the design"
---

# The `internal: true` carve-out is asserted, not enforced

## Goal

Make the credential boundary's one exemption **enforceable**, or remove it.

[C-312](C-312-connector-credential-boundary.md) put a credential boundary on plugin responses and
excused exactly one class: a host-dispatched `internal: true` op, on the grounds that it is never
advertised to the model and its result goes to host code rather than a transcript.
[C-403](C-403-the-broker-bypasses-the-credential-boundary.md) tried to audit that carve-out and
**asserted it was load-bearing for nothing**. Its independent review showed that claim is false, and
the way it failed is the whole argument for this story:

- The census lives in a **doc comment**. Nothing fails when it goes stale — and it was already wrong
  the day it was written. C-403's rewritten scope statement claimed four `call_with_host` sites; the
  tree has **five** non-test ones, and the omitted one is precisely the site the carve-out excuses.
- ⚠ **The carve-out is load-bearing right now**, for `plugin.validate` at
  `crates/flux-cli/src/plugin_cmd.rs:517` — production code, host-dispatched, `internal: true` via
  host-kit's auto-injection. Its result is **not** discarded: `problems`/`warnings` are lifted at
  `:533-534` and printed to operator stdout at `:555`, and its error frame is printed raw at `:539`.
  That is the same scrollback and shell-history surface C-312's own comment cites as the reason
  `flux plugin call` needs the check. The op's contract (`{operation, valid, problems}`) is benign
  today, but "benign" is a fact about the plugins that exist.
- **`internal_op` is public.** A plugin may ship an internal op that returns credentials — host-kit's
  own documentation describes precisely that shape with `aws-bedrock.auth`.

⚠ This is the shape this repo has been bitten by before: **a guard whose only check is its own
stated assumption**. A prose census is indistinguishable from a true one right up until it is wrong —
and here it took one independent read to find that it already was.

## Acceptance

- [ ] **Failing-first**: a test that ships an internal op returning credential material through the
      host-dispatch path and asserts the outcome the story decides on — failing at the merge base,
      where the material passes through.
- [ ] **Decide and implement one of**: apply the boundary to internal ops too (removing the
      carve-out entirely), or keep it and pin the census with a check that fails when a new
      host-dispatched `call_with_host` site appears without justification. Do not leave a third
      option where the comment is merely updated again.
- [ ] If the carve-out survives, the reason is stated **at the definition** and the enforcing test is
      named there, so the next reader finds the check rather than the claim.
- [ ] The check is verified to fire — reintroduce the violation it forbids and show it failing, the
      way C-391/C-392's scanner tests were validated before being trusted.
- [ ] Full gate green in both workspaces.

## Notes

- C-403's rewritten scope table in `crates/flux-plugin/src/host/credential_boundary.rs` is a
  starting inventory **known to have been wrong once**. Re-derive it from the tree
  (`git grep -n call_with_host -- '*.rs'`), never trust it; that is the whole point of this story.
- `crates/flux-cli/src/catalog_coherence.rs:173` builds a `HostProviderInvoker` with a fresh
  redactor. C-403 left it alone because it is a coherence-check helper with nothing registered — if
  this story changes how the boundary sources its redactor, re-check that site.
- A codegate lint would have to scan the nested `plugins/` workspace to see `internal_op` uses,
  which C-403 judged out of its scope. Decide whether that is the right enforcement point or whether
  a host-side runtime check is better placed.

## Progress

- Filed 2026-08-01 from C-403's audit, which fixed the discovery path and deliberately scoped this
  out rather than widening itself.
