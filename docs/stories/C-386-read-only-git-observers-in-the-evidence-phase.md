---
id: C-386
title: Decide how read-only Git observers reach the evidence phase
pillar: Agent
status: backlog
epic: agent-change-recovery-and-provenance
design: docs/designs/agent-change-recovery-and-provenance.md
note: "gather_safe allows only Read|Filesystem|Network effects and every git op declares Effect::Process, so git_status/git_log/git_diff/git_hunks are refused evidence-phase execution regardless of arguments — deliberate, documented in flux-spec, and coarse"
---

# Decide how read-only Git observers reach the evidence phase

## Goal

Resolve, deliberately, whether a fixed-argv read-only Git observer can produce evidence before
approval — or whether the harness must stop making exact-state claims it cannot support.

## Acceptance

- [ ] One of two mutually exclusive options is chosen and recorded in the design doc **before**
      implementation:
      **(a)** `StagingDisposition::Gather` loosens `gather_safe` for ops holding the coherence I1
      fixed-argv exemption — this breaks C-191's stated correspondence between `gather_safe` and
      `is_consequence_bearing_with_effects`, so `flux-spec` and its
      `gather_safety_stays_the_exact_negation_of_the_consequence_classifier` test must move in the
      same commit, per C-210's rule; or
      **(b)** capture is kept and the path returns a **typed** "evidence unavailable until approval"
      state instead of today's free-text `"captured as proposed action …"`, so exact-state claims
      become impossible to phrase.
- [ ] Failing-first tests for the chosen option: under (a), `git_status` returns repository text in
      the adaptive loop while `git_stage` under `Gather` is still refused; under (b), a completion
      asserting current HEAD after a captured `git_log` is rejected.
- [ ] Whichever is chosen, `crates/flux-spec/src/coherence.rs:104-135`'s statement of the outcome is
      updated to match.

## Progress

- 2026-08-01 — filed from validation of GIT-03. The classifier is argument-independent, so the
  reported behaviour is forced by the code rather than incidental.

## Notes

- **This touches the pre-approval execution envelope and is not a routine ergonomics story.**
- The review's own suggested fix ("classify capture by operation effect, not family or stage") is
  already how it works. The coarseness is that `Effect::Process` conflates "spawns a subprocess"
  with "acts".
