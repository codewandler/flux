---
id: C-211
title: "Validate the parent history before minting the fork's child session, and test the CLI refusal"
pillar: Core
status: in-progress
priority: 8
epic: typed-session-log
design: docs/designs/typed-session-log.md
note: "SURFACED BY the A-102 review — the refusal path is new, and both fork sites create the child before they know the parent is forkable; the CLI's copy of the logic has no test at all"
---

# Validate the parent history before minting the fork's child session, and test the CLI refusal

## Goal
A-102 gave `Session::fork` a refusal path: a parent whose log ends on an unanswered `tool_use` is
now rejected instead of copied through. Both fork sites build the child session *before* validating
the parent, so a refused fork leaves an empty orphan session behind — an artifact that did not exist
before the refusal path did. Reorder the two steps, and cover the CLI's independently-written copy
of the logic, which today has no test on its refusal branch at all.

## Acceptance
- [x] `Session::fork` (`crates/flux-sdk/src/session.rs:393-407`) validates the parent conversation
      into a `ValidHistory` **before** `create_session_with_context`, so a refused fork creates
      nothing. **Failing-first test**: fork a parent that ends mid-tool-pair, assert the refusal
      *and* that the store's session count is unchanged — today the count grows by one.
      → `crates/flux-sdk/src/session.rs:399` now precedes the mint at `:400`; pinned by
      `fork_refuses_before_minting_the_child_session` (`crates/flux-sdk/src/lib.rs:3144`).
- [x] The CLI fork (`crates/flux-cli/src/session.rs:300-318`) gets the same ordering.
      → `crates/flux-cli/src/session.rs:300` now precedes the mint at `:310`.
- [x] The CLI fork's **refusal branch** is exercised by a test. It is a second, hand-written copy of
      validate-then-rewrite with different error plumbing (`with_context` + `anyhow!` rather than
      `?`), so the SDK's test does not cover it — and a divergence between the two would surface as
      a CLI that still mints broken children.
      → `crates/flux-cli/tests/fork_refusal.rs:82`, which drives the real binary and asserts on the
      CLI's own "cannot be forked" context line, not the SDK's.
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — filed from the independent review of A-102, which confirmed the story's own acceptance
  in full (both failing-first tests verified against the merge base) and raised these two as
  deferrable minors rather than merge blockers.
- 2026-07-30 — done. Both fork sites hoist `ValidHistory::new` above `create_session_with_context`;
  the refusal now costs nothing.
  - Failing-first verified against the merge base (`cedef3f4`, `v0.36.0` + main) by reverting only
    the two production files: both tests fail on the session count, `left: 2 / right: 1`. The CLI
    test's *refusal* assertions already passed there — the refusal branch existed since A-102, so
    what this story pins is the ordering plus (newly) any regression in that branch's message.
  - The refusal stays a clean recoverable error, not a panic: the CLI exits non-zero with
    `error: session s_1 cannot be forked: tool_use orphan-1 at index 1 is never answered by a
    tool_result` (session id backquoted in the real output), asserted by the test.
  - `flux-sdk`'s public surface is unchanged — the `lib.rs` addition is inside `#[cfg(test)] mod
    tests` (1004–3236) and `fork`'s signature is untouched. Only its observable behaviour changes,
    in the fix's direction. `scripts/check-crate-versions.sh`: PASS (both crates inherit the
    workspace version, so they are out of that check's protocol-line scope).
  - **Not fixed, deliberately out of scope**: `WhatIf::run`'s re-plan path has the identical defect
    — `dst` is minted at `crates/flux-sdk/src/whatif.rs:440` and only validated at `:497`, and the
    `has no turn N to re-plan` bail at `:504-508` leaves the same orphan. The story's Acceptance
    names the two *fork* sites only. Worth its own story if "a failed operation leaves no trace" is
    to hold across the counterfactual paths too.

## Notes
- **Not urgent, and it is self-cleaning today**: nothing is appended to the orphan, so `last_seq == 0`
  and `prune_empty` collects it (`crates/flux-events/src/store/sqlite.rs:664-671`). The reason to fix
  it anyway is that "a failed operation leaves no trace" is cheaper to hold as an invariant than to
  re-derive from a pruning rule every time someone reads the fork path.
- The review also swept a third finding — `docs/designs/event-store-concurrent-use.md` rule R4 named
  the deleted `record_message` as a standing instruction — which was corrected directly during
  integration rather than filed here.
- Source: the A-102 review (2026-07-29); design: [typed-session-log.md](../designs/typed-session-log.md).
