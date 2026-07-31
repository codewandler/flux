---
id: C-380
title: Execute examples/commit.flux in a hermetic end-to-end test
pillar: Agent
status: backlog
epic: harness-route-integrity
design: docs/designs/harness-route-integrity.md
note: "examples_validate.rs is parse + lower against a NullProvider; rg 'commit\\.flux' finds no test that RUNS it. Nothing covers git output shapes, index refusal, approvals, commit creation or rollback"
---

# Execute `examples/commit.flux` in a hermetic end-to-end test

## Goal

Replace static validation with execution for the one example the project points at as its commit
workflow, so "the flow works" stops meaning "the flow parses".

## Acceptance

- [ ] A hermetic temp-git-repo integration test drives the flow through the public flow route with a
      scripted approver, covering: clean-index success, pre-staged-index refusal, explicit-path
      isolation, invalid title, invalid body, and declined approval.
- [ ] The test asserts `git_push` never dispatches.
- [ ] It lives where a full `Executor` plus approver assembles (`flux-flow` or `flux-cli` tests), not
      in `flux-eval`'s parse/lower sweep.
- [ ] The `examples_validate` sweep's module documentation states plainly that it proves parsing and
      lowering only.

## Progress

- 2026-08-01 — filed from validation of HAR-03.

## Notes

- The review's other recommendation — labelling static checks as static in agent-visible results —
  has no target: there is no model-facing validate/lower op at all. The overclaim in the reported
  session was agent prose over a `cargo test` invocation. C-378 is where a labelled result would live.
