---
id: A-05
title: Legible silent-success feedback — stop the loop re-running ops that already succeeded
pillar: Agent
status: ready
priority: 3
note: a zero-output success feeds back as `[bash]\n` + nothing — indistinguishable from "didn't run", so the model re-plans the identical op; observed 3 model calls / 13.5s / $0.14 for one `rm`
---

# Legible silent-success feedback — stop the loop re-running ops that already succeeded

## Goal
Make a successful zero-output op legible in the loop feedback so the model doesn't re-run it.
Observed live (2026-07-01, session s_311): `flux run --yes "delete the file victim2.txt"` →
plan 1 `bash("rm victim2.txt")` succeeds (✓, empty stdout) → plan 2 emits the **byte-identical**
command (fails: file already gone) → plan 3 checks `path_exists` and finally answers. Three planner
calls, 13.5s and $0.14 for a one-op task; the same first plan also ran `rm` **twice** in the
uninstrumented variant.

Root cause: `BashTool::execute` returns `content: ""` for a silent success (stdout/stderr empty,
`[exit N]` appended only when `N != 0` — `crates/flux-tools/src/lib.rs`), and the flow transcript
renders a bare call as `[{op}]\n{view}` (`crates/flux-lang/src/runtime.rs:1142,1160`) — so the
loop-carried `$feedback` for "rm succeeded" is `[bash]` followed by a blank line. The model cannot
distinguish that from "nothing happened" and retries. (Same failure family as the fixed
empty-parallel-branch bug: empty evidence spins the loop.)

## Acceptance
- [ ] Failing-first: a plan whose only op succeeds with empty output produces a `run_plan`
      transcript containing an explicit success marker (e.g. `[bash] ✓ exit 0 (no output)` /
      `(ok — no output)`), asserted at the transcript-renderer level so it covers EVERY op with an
      empty view, not just `bash`.
- [ ] `BashTool` success output includes the exit status when the body is empty (`[exit 0] (no
      output)`), so even raw content consumers see it.
- [ ] Loop-guard extension: an identical plan re-emitted immediately after that plan **succeeded**
      is fingerprinted (today only identical failures / identical transcripts stall) — one escalate
      nudge, "the previous identical plan succeeded; do not re-run it".
- [ ] Live re-check: the `rm` scenario completes in 2 provider calls (one plan + one prose/complete
      round), not 3, and the command executes exactly once.

## Progress
- (not started)

## Notes
- Found during the 2026-07-01 harness e2e review.
- Interacts with A-04: with shell gated off the exact repro changes, but the class (any silent
  success — `write` to an existing path, an op returning empty list, …) stays.
- Cheapest correct fix is in the transcript renderer (one place, all ops); the BashTool marker is
  belt-and-braces.
