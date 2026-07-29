---
id: C-131
title: flux policy simulate — replay a proposed policy against recorded history
pillar: Core
status: in-progress
priority: 5
epic:
design:
note: "before adopting a policy edit, replay it over the last N sessions' recorded ops: 'this change would have blocked these 12 ops and newly-allowed these 3', as a diff-style report; pure read over the event log + existing policy evaluator; the trust-builder for approval distillation (C-94)"
---

# flux policy simulate — replay a proposed policy against recorded history

## Goal
Let an operator trust a policy change before adopting it: `flux policy simulate <proposed.toml>`
replays the proposed policy against the recorded op history and reports, diff-style, which
historical ops it would have newly blocked and newly allowed relative to the active policy.

## Acceptance
- [x] `flux policy simulate <file> [--sessions N]` evaluates both the active and proposed policy
  against recorded op requests and prints newly-blocked / newly-allowed / unchanged counts with
  per-op detail — failing-first test over a seeded event store.
- [x] Pure read: simulation writes nothing to the event store and constructs no providers.
- [x] Ops whose recorded context is insufficient to re-evaluate are reported as
  "indeterminate", never silently classified.
- [x] `--json` output for tooling.

## Progress
- Implemented as a CLI-only subcommand — it registers no op, so the public op catalog and
  `website/docs/language/ops.md` are untouched.
- `crates/flux-cli/src/policy_cmd.rs` holds the replay; `crates/flux-cli/tests/policy_simulate.rs`
  is the acceptance suite (4 tests, drives the real binary over a seeded store), plus 8 unit tests
  in `policy_cmd.rs`.
- Indeterminacy is three-sourced and each carries a reason: no authority contract for the op in
  this build, a record missing its caller, and a verdict that turns on caller trust/scopes/groups
  the log never recorded. The third is scoped — a missing fact only makes indeterminate the ops it
  could actually have decided, so an inert gate still leaves every op decided.
- Known limit, surfaced in the report's `assumes:` line rather than hidden: the log records a
  caller's principal id but not its kind, so every recorded caller is replayed as a `user`
  principal. A proposed policy whose subjects discriminate on principal *kind* is refused whole
  rather than answered wrongly (`a_kind_dependent_subject_refuses_the_whole_replay`).

## Notes
- Pairs with approval distillation (C-94): distillation proposes a policy; simulation lets you
  trust it before adoption.
