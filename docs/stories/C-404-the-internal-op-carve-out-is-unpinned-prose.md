---
id: C-404
title: "The credential boundary's `internal: true` carve-out is prose with no test pinning it, and `internal_op` is public"
pillar: Core
status: ready
priority: 10
epic: connector-platform
areas: [flux-plugin, flux-cli, flux-codegate]
note: "found by C-403, which rewrote the carve-out into a dated statement of fact and then said plainly that nothing enforces it. host-kit's own docs describe an internal op returning credentials (`aws-bedrock.auth`), so the benign census is a property of today's plugins, not of the design"
---

# The `internal: true` carve-out is asserted, not enforced

## Goal

Make the credential boundary's one exemption **enforceable**, or remove it.

[C-312](C-312-connector-credential-boundary.md) put a credential boundary on plugin responses and
excused exactly one class: a host-dispatched `internal: true` op, on the grounds that it is never
advertised to the model and its result goes to host code rather than a transcript.
[C-403](C-403-the-broker-bypasses-the-credential-boundary.md) audited that carve-out, found it
covered nothing in this tree, and rewrote it as a dated statement of fact — then flagged the real
problem:

- The census lives in a **doc comment**. Nothing fails when it goes stale.
- **`internal_op` is public.** A plugin may ship an internal op that returns credentials —
  host-kit's own documentation describes precisely that shape with `aws-bedrock.auth`.
- The one live host-dispatched site today, `plugin.validate`
  (`crates/flux-cli/src/plugin_cmd.rs:517`), is genuinely benign: its contract is
  `{operation, valid, problems}`. That is a fact about the plugins that exist, not a property the
  design guarantees.

⚠ This is the shape this repo has been bitten by before: **a guard whose only check is its own
stated assumption**. A prose census with no test is indistinguishable from a true one right up until
it is wrong.

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

- C-403's rewritten scope table in `crates/flux-plugin/src/host/credential_boundary.rs` is the
  starting inventory — it names four `call_with_host` sites. Re-derive it rather than trusting it;
  that is the whole point of this story.
- `crates/flux-cli/src/catalog_coherence.rs:173` builds a `HostProviderInvoker` with a fresh
  redactor. C-403 left it alone because it is a coherence-check helper with nothing registered — if
  this story changes how the boundary sources its redactor, re-check that site.
- A codegate lint would have to scan the nested `plugins/` workspace to see `internal_op` uses,
  which C-403 judged out of its scope. Decide whether that is the right enforcement point or whether
  a host-side runtime check is better placed.

## Progress

- Filed 2026-08-01 from C-403's audit, which fixed the discovery path and deliberately scoped this
  out rather than widening itself.
