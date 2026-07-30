---
id: C-218
title: "`git_diff` honours external diff drivers, so its I1 exemption claims more than it can hold"
pillar: Core
status: done
priority: 8
epic: security-assurance
design: docs/designs/security-assurance.md
note: "the exemption's stated grounds are 'argv is fixed by the op, never a caller-supplied program name' — but `git diff` without --no-ext-diff runs whatever diff.external names, so fixed argv does not mean fixed behaviour"
---

# `git_diff` honours external diff drivers, so its I1 exemption claims more than it can hold

## Goal
`flux_spec::coherence::EXEMPT` excuses `git_status`, `git_diff` and `git_log` from invariant I1 — the
risk floor — so they may keep `Risk::Low` despite declaring `AccessKind::Process`. The stated
justification is precise, and it is the whole basis of the exemption:

> their argv is fixed by the op (`git status --short`, `git diff`, `git log`), never assembled from a
> caller-supplied program name; the caller may only narrow the scope (a path, a limit).

For `git_status` and `git_log` that holds — verified. For **`git_diff` it does not**, because fixed
argv is not the same as fixed behaviour. `git diff` consults `diff.external` and per-path
`.gitattributes` diff drivers (`diff.<driver>.command`), and when either is configured **git executes
that program instead of diffing internally**. The op's argv never changes; what runs does.

Close the gap by passing `--no-ext-diff`, and make the exemption's justification true again.

## Acceptance
- [x] `GitDiffTool`'s argv passes `--no-ext-diff --no-textconv` (`crates/flux-tools/src/lib.rs`, the
      `vec!["git", "diff"]` construction). **Failing-first test**: in a fixture repo configured with
      `diff.external` pointing at a marker program, assert `git_diff` does *not* execute it — the
      test fails today because it does.
- [x] Whole-file and hunk diff paths pass `--no-textconv`; a matching `.gitattributes` textconv
      driver fixture proves the low-risk operation cannot execute it.
- [x] Audit the sibling `git_*` ops for the same class of config-directed execution and state the
      result in the story, rather than assuming. `git status`/`git log` do not run diff drivers on
      their default argv, but `git log` gains one under `-p`/`--ext-diff`, so state explicitly
      whether any caller-reachable flag can get there.
- [x] The `EXEMPT` entry's `reason` for `git_diff` is updated to say *why* the argv is now genuinely
      behaviour-fixing — the reason string is read by `the_allowlist_is_well_formed` and is the only
      place the justification lives.
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — found while reviewing C-92, by comparing its new `git_hunks` op against the shipped
  `git_*` family. The new op passes `--no-ext-diff`; the existing `git_diff` does not. The
  discrepancy is what surfaced it.
- 2026-07-30 — failing-first Unix fixture configured `diff.external` to a marker program and proved
  the low-risk op executed it. `GitDiffTool` now always passes `--no-ext-diff`; the same fixture
  proves the external program stays untouched, and the I1 exemption names the behaviour-fixing flag.
- 2026-07-30 — sibling audit: `git_status` exposes no caller flags and runs only `git status
  --short`; `git_log` exposes only a numeric limit and runs `git log -N --oneline`, so neither can
  enable patch/external-diff behaviour. `git log` would become suspect if `-p` or `--ext-diff` were
  ever made caller-reachable.
- 2026-07-30 — full `codewandler-flux-tools` and `codewandler-flux-spec` tests plus scoped clippy
  with `-D warnings` are green.
- 2026-07-30 — closure review covered Git's separate text-conversion seam: whole-file and hunk diff
  argv now pass `--no-textconv`, and a repository fixture proves a matching textconv driver cannot
  execute through the low-risk op.

## Notes
- **Scope the severity honestly, in both directions.** This is *not* remote code execution from
  cloning a hostile repository: `git clone` does not import a remote's `.git/config`, so an attacker
  cannot set `diff.external` merely by getting you to clone. It needs local git config to already
  name a program — a repo the user configured that way, a config a prior agent wrote, or a checkout
  that shipped its own `.git` directory.
- **What makes it worth fixing anyway** is the tier, not the likelihood. `Risk::Low` is what
  `RiskApprover` auto-approves, so `git_diff` runs without a prompt. An op that can be redirected to
  an arbitrary program while sitting at the auto-approved tier is precisely the shape I1 exists to
  forbid — and the exemption is what holds it there. Either the justification is true or the
  exemption should not exist; today it is neither.
- Not pre-approval reachable: `Effect::Process` makes `git_diff` consequence-bearing for
  `gather_safe` regardless of the I1 exemption, and the exemption comment already records that it
  "does not reach the two guards that matter most: `gather_safe` and the op cache". That containment
  is intact and is why this is a story rather than an incident.
- The fix is one flag. The value is in the *test* and in the corrected reason string: without them
  the next `git_*` op added will reproduce the same assumption.
