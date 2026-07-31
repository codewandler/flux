# Assurance lane residuals — 2026-08-01

## Context

`ASSURE-01` is a compound claim and the validation pass split it lane by lane rather than letting
one addition close the whole thing.

| Lane | State on 2026-08-01 |
| --- | --- |
| CodeQL (`security-extended`) | Exists, runs weekly and on push, **last analysis green-but-non-blocking with 13 open `critical` alerts** |
| Deterministic adversarial corpus (smoke) | Exists, runs on push, genuinely non-vacuous (self-test rejects disabled and comment-only decoys) |
| Deterministic corpus (deep) | Declared, `schedule`-only, **never executed** |
| Miri | Declared over two pure seams, `schedule`/`dispatch`-only, **never executed** |
| `cargo-deny` / `cargo-audit` | Exists and green — but all 43 runs are `event=push`; the weekly cron **has never fired** |
| Release attestation | Exists and verified live |
| Fuzzing | **Absent.** The "adversarial corpus" is a seeded deterministic generator over committed fixtures, not coverage-guided, and keeps no persistent corpus |
| Sanitizers (ASan/TSan/MSan) | **Absent** |

So: the SAST, Miri and attestation limbs are historical-fixed, the fuzzing and sanitizer limbs are
reproduced, and a third of the declared surface has never run at all.

## Finding-to-story traceability

| Residual | Story |
| --- | --- |
| 13 untriaged `critical` CodeQL alerts on a lane whose job succeeds regardless | C-359 |
| Miri, `corpus-deep` and the weekly dependency audit are declared but unexercised | C-360 |
| No coverage-guided fuzzing and no persistent corpus | C-361 |
| No sanitizer lane over the unsafe-adjacent and parser seams | C-362 |

## Decisions

- **A declared lane that has never run is not a lane.** Proof-of-life comes before broadening
  coverage: force a dispatch and record the result before the first cron fires.
- **Alerts are triaged or dismissed with a recorded reason — never left open on a non-blocking
  job.** An open `critical` that blocks nothing trains everyone to ignore the lane.
- **A deterministic corpus and a fuzzer are different instruments.** The corpus proves known
  adversarial shapes stay handled; the fuzzer searches for unknown ones and must keep what it finds.
  Neither substitutes for the other, and the changelog should stop implying otherwise.

## Closure proof

Each lane reports a real execution — a run id, a duration, and a finding count — before its story
closes. `corpus-deep` and Miri close only on a completed scheduled (not dispatched) run.
