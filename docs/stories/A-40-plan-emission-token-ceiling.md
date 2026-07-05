---
id: A-40
title: Oversized plan emission dies at max_tokens — split, don't retry the whole plan
pillar: Agent
status: backlog
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: "I-03's tbench regression signature: execute-phase plans on write-heavy tasks truncate at the 16384 emission ceiling and the loop re-pays whole-plan retries (31 steps / $0.76 per fibonacci-server trial, 4× baseline) without ever completing"
---

# Oversized plan emission dies at max_tokens — split, don't retry the whole plan

## Goal
I-03's terminal-bench post leg failed both tasks with the same mechanical signature: `planner
output was truncated at max_tokens (16384) before it finished the plan — raise --max-tokens or
split the request into smaller steps`, retried repeatedly at full price. A plan that cannot fit
the emission ceiling should get *smaller* on retry (fewer steps now + a continuation turn, or
payload-bearing steps split out), not be re-emitted whole until the budget or the step cap dies.
The design doc's "I-03 measurement results" section carries the measured evidence.

## Acceptance
- [ ] Truncated-at-max_tokens plan emission is detected as its own repair class (it already
      surfaces as a distinct runtime error) and the repair prompt instructs a *split*: emit the
      plan's first N statements + explicit continuation intent, or hoist large literal payloads
      (file writes) into their own follow-up plan — failing-first test on the repair path.
- [ ] A second truncation on the already-split plan does not loop: bounded retries with the
      existing stall/budget guards, then a legible failure naming the ceiling.
- [ ] `"""` multi-line strings (L-39) are used by the emission prompt's guidance for large write
      payloads so the JSON-escaping bloat stops inflating token counts (planner grammar already
      teaches the spelling — verify the repair guidance references it).
- [ ] The I-03 fibonacci-server scenario (write server.js + start + verify, 16384 cap) completes
      on the phased loop in a harness re-run or an equivalent eval fixture; measured
      before/after cost of the failure mode recorded here.
- [ ] Gate green.

## Progress
- (not started)

## Notes
- Evidence: `bench/tbench-compare/results/i03-go/post-report.txt` (fibonacci-server 31 steps /
  22.9k out tokens / $0.7553 per trial, all checks failed; chess sub-agent turn lost to the same
  truncation). Baseline completed equivalent plans under the same 16384 ceiling — the phased
  loop's execute-phase plans are bigger.
- Related: A-30/A-31 emission-repair machinery (repair rides cached segments), L-39 multi-line
  strings, C-35 (the gather-round cache economics of retries).
