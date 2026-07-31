---
id: C-349
title: Close the core.fsmonitor execution seam on the exempt git argv
pillar: Core
status: backlog
epic: egress-pinning-and-confinement-residuals
design: docs/designs/egress-pinning-and-confinement-residuals.md
note: "REPRODUCED TODAY — with core.fsmonitor set, an arbitrary program executes under both git_status and git_diff; two Risk::Low, I1-exempt, auto-approved observers. C-218's sibling audit was scoped to diff drivers and missed it"
---

# Close the `core.fsmonitor` execution seam on the exempt git argv

## Goal

Finish the job C-218 started: a fixed-argv git observer classified `Risk::Low` must not be able to
execute a program named by repository or user git configuration.

## Acceptance

- [ ] `git_status` (`crates/flux-tools/src/lib.rs:2293`) and `git_diff` (`:2367`) neutralise
      `core.fsmonitor` (e.g. `-c core.fsmonitor=false`, or `--no-optional-locks` where it applies),
      alongside the existing `--no-ext-diff --no-textconv`.
- [ ] The audit is re-run across the whole exempt git argv set — `git_log`, `git_hunks`, and the
      non-op git invocations in `flux-eval` and `flux-runtime` — for any other config-directed
      execution seam, and the result is recorded rather than assumed.
- [ ] The I1 exemption reasons in `crates/flux-spec/src/coherence.rs:115-125` name the full set of
      seams they close; the strings currently claim more than they hold.
- [ ] Failing-first regression: a temp repo with `core.fsmonitor` pointing at a marker program,
      asserting the marker does not run under either op. The existing C-218 fixtures are the model.
- [ ] The C-218 regressions and the new one are not `#[cfg(unix)]`-only, or the Windows gap is
      recorded explicitly.

## Progress

- 2026-08-01 — reproduced empirically during validation (git 2.55.0): the program runs under
  `git status --short` and under `git diff --no-ext-diff --no-textconv`, but not `git log --oneline`.

## Notes

- Precondition class is identical to C-218: local or user git config must already name a program,
  and `HOME` is forwarded by `SAFE_ENV`, so `~/.gitconfig` counts.
