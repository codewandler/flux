---
id: C-747
title: "A branch retained as durable storage says so where a reaper can read it"
pillar: "Core"
status: backlog
priority: 2
epic: delivery-is-verified
areas: [flux-cli]
---

# A branch retained as durable storage says so where a reaper can read it

## Goal

Main's own documents use branch names as durable storage. `C-688` says a branch *"is retained
indefinitely, so nothing is lost by deferring it"*; `D-232` says *"resume from that preserved branch
and its live-test runbook"*; `D-237` pins a dependency decision to a specific branch tip sha.

None of that is machine-readable. Those branches survived today's audit only because the auditor
grepped every branch name against main's tree. Any reaper without that step destroys them — and
`impl/D-232` alone holds 1,692 lines that have never existed on main.

## Acceptance

- [ ] A branch retained as durable storage declares it where a tool can read it — a frontmatter field
      on the story that retains it, or an annotated tag — not only in prose.
- [ ] Any reaper refuses a branch so marked, and says which document retains it.
- [ ] The existing retentions are migrated: `impl/D-232`, `fleet/wave-299/flux/story/C-569`,
      `fleet/wave-302/flux/story/C-569`, `preserve/quiet-flow-pre-sync-20260805`,
      `wave/flux-exchange-lifecycle-1`, `wave/flux-exchange-lifecycle-2`, `impl/C-217`.
- [ ] `board check` reports a declared retention whose branch no longer exists, so the claim cannot
      quietly become false.
- [ ] Regression test: a marked branch survives a reap that would otherwise take it, and the marker
      is reported when the branch is missing.
