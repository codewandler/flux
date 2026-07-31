---
id: C-312
title: "The credential boundary — prove a vendor credential never enters flux"
pillar: Core
status: ready
priority: 9
epic: connector-platform
areas: [flux-plugin, flux-secret]
note: "the connectors seam's central invariant, asserted rather than assumed: flux holds exactly ONE secret on this path — the deployment session bearer. A response carrying credential-shaped material is refused, not merely redacted"
---

# The credential boundary — prove a vendor credential never enters flux

## Goal

Make "flux never holds a vendor credential" a property the tree enforces, not a claim the design
makes. On the connectors seam the deployment resolves and injects the vendor credential; flux holds
exactly one secret — the deployment's own session bearer — and that asymmetry is the entire safety
argument for the seam.

An invariant nobody tests is an invariant that decays at the next refactor. This story is the test.

## Acceptance

- [ ] **Failing-first test**: a platform-sourced operation whose response carries credential-shaped
      material is **refused**, not merely redacted. Redaction hides a leak from the model; refusal
      says the boundary was crossed. State which shapes are recognised and why that set, not a
      different one.
- [ ] Platform-sourced ops carry an empty `secret_purposes` — the deployment resolves credentials, so
      flux must not be asked to. A manifest that declares `secret_purposes` on a platform-sourced op
      is refused at load, with the test naming it.
- [ ] The activation / auth-initiation path returns **a URL for a human** and never a token. Prove
      the negative: a response containing token material where an authorize URL was expected is
      refused.
- [ ] **Responses are treated as out-of-jail input**, because they are: injection-shaped,
      secret-bearing, and authored by whatever the deployment talked to. Redact and escape **at
      ingest**, not at display, and **reuse C-215's machinery** rather than growing a second
      redaction path — C-215 established exactly this posture for harness transcripts, and its own
      review found the ingest bound it asserted was not the bound its code had. Do not repeat that.
- [ ] A test asserts no vendor credential appears in the session log, the evidence log, or a tool
      result, for a full activate → refresh → dispatch journey against a fixture.
- [ ] Full gate green in both workspaces.

## Progress
- Filed 2026-07-31 from the approved connectors-seam plan.

## Notes
- The one secret flux *does* hold on this path is the deployment session bearer, and it is stored like
  any other credential. `flux auth login connectors` already supplies it —
  `crates/flux-cli/src/auth_cmd.rs:112-121` falls through to `login_plugin` for any non-builtin name.
- The confused-deputy question, answered honestly: `../flux-connectors/docs/designs/connectors-proxy.md`
  names it — *"a credential-injecting proxy is, by construction, a confused-deputy machine: its entire
  job is to add authority a caller does not have."* That design was **superseded** by
  `connectors-app.md`, which carves out the narrow defensible case: one operator, their own
  credentials, a process they started, loopback-bound. This story's tests are what keep flux's half
  inside that carve-out.
- Sibling stories: [C-310](C-310-plugin-catalog-refresh.md),
  [C-311](C-311-vendor-host-disclosure-at-approval.md). All three touch
  `crates/flux-plugin/` — run them in separate waves.
