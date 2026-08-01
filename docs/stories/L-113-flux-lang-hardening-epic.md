---
id: L-113
title: "flux-lang hardening — remediate the 2026-08-01 subsystem review (epic)"
pillar: Language
status: ready
priority: 5
epic: flux-lang-hardening
design: docs/designs/flux-lang-hardening.md
areas: [flux-lang]
note: "EPIC — every finding of the 2026-08-01 flux-lang review owned: two parser totality bugs, interpreter budgets, confirm intents, mirror debt, fuzzing"
---

# flux-lang hardening — remediate the 2026-08-01 subsystem review (epic)

## Goal

Close every finding of the flux-lang subsystem review
(docs/reviews/single/2026-08-01-flux-lang-subsystem-review.md): restore the parser's totality
claims (no abort, total round-trip), give the interpreter the budgets its own doc-comments promise,
make `confirm` approvals carry machine-checkable intents, and put the assurance floor (fuzzing,
mirrors) under the language surface.

## Acceptance

- [ ] L-114 and L-115 close the two HIGH findings (statement-depth abort; `each` `->` text-split)
      with failing-first tests on the previously untested axes.
- [ ] L-116 gives `repeat` the same budget/transcript/yield discipline as `loop`, and settles the
      per-execution vs per-activation budget semantics with a recorded decision.
- [ ] L-117 makes `confirm` approvals intent-bearing (or records why the label-only contract is
      the intended seam).
- [ ] L-118 converts the standing tree-sitter red into owned, deadline-bearing work.
- [ ] L-119 adds raw-text fuzzing and input-size bounds to the parser front-end.
- [ ] L-120 clears the LOW/INFO drift batch.
- [ ] The review's triage block is updated to `handled` with these stories as owners once all land.

## Progress

- 2026-08-01: Epic opened from the subsystem review; user asked for all findings tracked with
  suggested fixes.

## Notes

- The review's meta-finding governs the fixes: F1/F2 are the fourth instance of "guard tested
  against its own assumptions" — every fix here must add its test on the axis that was *missing*,
  not re-prove the covered one.
- Child stories: L-114 … L-120.
