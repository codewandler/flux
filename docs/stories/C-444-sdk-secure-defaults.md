---
id: C-444
title: "`auto_approve(true)` does not imply confinement, and SDK resource ceilings are unbounded by default"
pillar: Core
status: ready
priority: 3
design: docs/designs/pi-comparison-remediation.md
epic: pi-comparison-remediation
areas: [flux-sdk]
note: "⚠ the finding that undercuts flux's headline claim. Both are DOCUMENTED, and documented is not defaulted — an embedder reading the headline and not the caveat gets auto-approval with no confinement and no ceiling, which the review itself calls a poor fit"
---

# Documented is not defaulted

## Goal

Make it impossible for an embedder to reach auto-approval with no OS confinement and no resource
ceiling by following the documented happy path.

## The two findings

From the 2026-08-01 Pi comparison, both line-cited:

- **F2** — *"The SDK also states that `auto_approve(true)` does not imply confinement; the embedder must
  set it"* (`crates/flux-sdk/src/lib.rs:17`).
- **F4** — *"The SDK's runtime-use ceilings are unbounded by default and per agent; a delegated tree can
  multiply its concurrent tool count"* (`crates/flux-sdk/src/lib.rs:792`).

⚠ flux's argument is that authorization and approval are **runtime types that cannot be disabled**.
These two are where an embedder falls out of that without noticing — and it is why the review scores
Embeddability 8.0 against Pi's 9.0 with the reading *"asks more of the embedder."* That phrase is
usually about ergonomics; here it is about safety.

## Acceptance

- [ ] **Failing-first**: a test constructing an SDK agent the documented way with `auto_approve(true)`
      and asserting it is confined and has a resource ceiling — failing at the merge base.
- [ ] ⚠ **Decide and record: does `auto_approve(true)` imply confinement, or refuse without an explicit
      confinement decision?** Both are defensible; silently doing neither is not. The CLI's precedent is
      C-410 — unattended surfaces fail *closed* — and an SDK embedder is the same posture with no human
      at a terminal.
- [ ] A default resource ceiling exists, and ⚠ **a delegated tree cannot multiply past it** — per-agent
      ceilings that compose into an unbounded total are the actual finding, not the per-agent number.
- [ ] ⚠ **This is a breaking change for embedders and owes a MINOR** under the pre-1.0 rule. Existing
      embedders get behaviour they did not ask for; that is the point, but it must be deliberate, in the
      CHANGELOG, and in WHATS-NEW with an action line.
- [ ] The SDK docs stop *warning* about the gap and describe the new default. A caveat that is no longer
      true is worse than one that is.
- [ ] Full gate green.

## Notes

- ⚠ Highest priority in this epic: it is the only finding where flux's own headline claim is weaker than
  it reads.
- The escape hatch matters as much as the default — an embedder who genuinely wants no confinement must
  be able to say so explicitly, and that call should be visible in their code.
- Related: C-410 raised the unattended CLI surfaces to fail-closed; this is the same argument one layer
  out.

## Progress
- Filed 2026-08-02 from the Pi comparison.
