---
id: C-404
title: "The credential boundary's `internal: true` carve-out is prose with no test pinning it, and `internal_op` is public"
pillar: Core
status: done
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

- [x] **Failing-first**: a test that ships an internal op returning credential material through the
      host-dispatch path and asserts the outcome the story decides on — failing at the merge base,
      where the material passes through.
      → `crates/flux-cli/tests/plugin_preflight_boundary.rs`'s
      `a_platform_sourced_preflight_carrying_a_vendor_credential_is_refused`, driving the real
      `flux plugin call --dry-run` binary against `platform_plugin`'s new `leak-validate` mode (an
      `internal: true`, `platform`-sourced `plugin.validate`). At `def26f35` it printed
      `xoxb-…` to operator stdout.
- [x] **Decide and implement one of**: apply the boundary to internal ops too (removing the
      carve-out entirely), or keep it and pin the census with a check that fails when a new
      host-dispatched `call_with_host` site appears without justification. Do not leave a third
      option where the comment is merely updated again.
      → **Both.** The carve-out is removed (`crates/flux-cli/src/plugin_cmd.rs:535`, the preflight
      verdict; `:565`, its error frame), *and* the census is pinned by
      `flux-codegate`'s `every_plugin_response_ingest_site_is_in_the_credential_boundary_census`.
      Removing the carve-out alone would have left `secret.read` exempt behind the same unenforced
      prose.
- [x] If the carve-out survives, the reason is stated **at the definition** and the enforcing test is
      named there, so the next reader finds the check rather than the claim.
      → It does not survive. The one remaining exemption (`secret.read`, exempt *by purpose*, not by
      dispatcher) is stated in `credential_boundary.rs`'s header, which now cites the census test by
      name instead of carrying a table.
- [x] The check is verified to fire — reintroduce the violation it forbids and show it failing, the
      way C-391/C-392's scanner tests were validated before being trusted.
      → Both failure branches were reintroduced and observed red, then restored: a new
      `call_with_host` in an uncensused file (`flux-plugin/src/host.rs`) and a second one in a
      censused file (`flux-cli/src/plugin_cmd.rs`).
- [x] Full gate green in both workspaces.

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
- Implemented 2026-08-01 on `impl/C-404`, off `def26f35`.
  - **Census re-derived from the tree, not from the table.** `git grep -n call_with_host -- '*.rs'`
    gives six non-test `.call_with_host(` expressions in three files: `broker.rs` ×2,
    `plugin_cmd.rs` ×2, `loading.rs` ×2. C-403's five-row table was correct as of this commit —
    which was never the point. The scanner now derives the same six with `syn` and fails when the
    number moves.
  - **C-403's "wiring no test can observe" argument was half right and is answered rather than
    inherited.** `host_kit::internal_op` really does yield `PlatformSourcing::None`, so the check is
    a no-op on every plugin in this repository. But `host-kit` is a convenience, not the protocol:
    a plugin speaking raw NDJSON can declare `plugin.validate` with `platform` set, and the fixture
    now does exactly that. So the wiring *is* observable, and the failing-first test observes it end
    to end through the real binary rather than through `refuse_platform_response` directly — the
    story is about a wiring claim, and a helper-level test would have re-asserted the helper.
  - **The error path is scrubbed, not escalated.** A preflight that errors has always been
    non-fatal (plugins predating D-88 do not serve the op), so `scrub_plugin_error` replaces the
    message and the schema-only fallback is kept. Escalating it would change `--dry-run`'s contract
    for a reason this story does not carry.
  - `crates/flux-cli/src/catalog_coherence.rs:173` was re-checked per the story's note: it is
    untouched. Nothing here changes how the boundary sources its redactor.
