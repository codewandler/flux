# Adaptive intent latency

**Status:** measured; candidate rejected (A-78)  
**Date:** 2026-07-14  
**Extends:** [adaptive-loop hardening](adaptive-loop-hardening.md)

## Question

The mandatory intent call is deliberately a model decision, but it should not inherit expensive
reasoning or output capacity without evidence that those settings improve routing. The experiment
asks whether a 512-token cap, with or without low reasoning effort, reduces the visible initial
delay while preserving the A-73 cross-model correctness result.

## Arms and workloads

The universal arms are current inheritance with 1,024 output tokens, inheritance with 512, and low
effort with 512. A fourth diagnostic uses Gemini 3.5 Flash for intent under an OpenRouter DeepSeek
parent; same-provider validation stays mandatory and this arm cannot become a universal default.

Screening is three trials per universal arm and model on the adversarial support fixture. Baseline
and the provisional winner then receive five fresh trials per model on pure conversation, current
time through `now`, and support retrieval. One Bitcoin-to-Slack approval-denial smoke per model pins
integration routing and no-write behavior. Every evaluator invocation explicitly uses a 12-call
limit so the new production default cannot turn a failed experiment into uncontrolled spend.

## Keep gate

Correctness is lexicographically first. A candidate must pass every deterministic grader, add no
intent repair or provider call, lower median intent latency by at least 20%, lower greeting/time
end-to-end latency by at least 10%, and keep support latency within 5% of baseline. A tie chooses the
smaller behavior change: baseline, cap-only, then low-effort/cap. Failure to qualify is a valid
result: the evaluator and report land, but the shipped intent defaults remain unchanged.

Only redacted `model.call`, approval, execution, and wall-clock measurements enter the report. Full
provider bodies and private reasoning are neither required nor retained.

## Evaluator

[`scripts/eval-adaptive-latency.sh`](../../scripts/eval-adaptive-latency.sh) materializes an
adversarial disposable support workspace and runs the complete CLI path. It records only timestamped
normal CLI output, summary model traces, numeric event projections, the final answer needed by the
grader, and session/log pointers. It never enables `FLUX_MODEL_TRACE=full`. Each invocation passes
`--max-model-calls 12` explicitly, so the A-77 production default cannot conceal extra calls.

The confirmation runner alternates arm order inside every model/workload/trial pair (AB for odd
trials, BA for even trials). An initial arm-at-a-time confirmation pilot was stopped when provider
drift became visible and is excluded from every conclusion below; its rows remain labeled `confirm`
for auditability, while valid rows are labeled `confirm_paired`.

The support grader originally failed two semantically correct phrasings: “Seats remaining: 3” and
“3 minute”. The matcher was broadened before paired confirmation, and the two affected screening
rows were regraded from their retained answers. No model output was edited and no paired result was
manually relabeled.

## Results

All 36 screening turns passed after that grader correction. The pooled median intent times were
2,548 ms for baseline, 2,627 ms for 512 tokens, and 2,676 ms for low effort plus 512 tokens. The
512-token arm was provisionally advanced because it was the smaller change and helped GPT-5-mini in
screening, not because it had yet met the keep gate.

Paired confirmation ran 120 fresh turns: five trials of baseline and cap-only for each of four
models and three workloads. The model-level result is:

| Model | Baseline pass | 512 pass | Baseline intent median | 512 intent median | Change |
|---|---:|---:|---:|---:|---:|
| Codex gpt-5.5 | 15/15 | 15/15 | 2,204 ms | 2,241 ms | +1.7% |
| GPT-5-mini | 15/15 | 11/15 | 4,773 ms | 4,320 ms | -9.5% |
| DeepSeek V4 Flash Nitro | 14/15 | 14/15 | 4,548 ms | 3,851 ms | -15.3% |
| Gemini 3.5 Flash | 14/15 | 15/15 | 1,136 ms | 1,059 ms | -6.8% |

No model reached the required 20% intent reduction. The cap added GPT-5-mini intent-contract
failures and repairs, regressed several end-to-end workload medians, and increased Codex provider
calls from 45 to 46 across its 15 turns. The strict gate therefore returns `REJECT cap512`; the
shipped intent token and effort defaults remain unchanged.

The baseline misses are useful product findings rather than evidence for the cap. DeepSeek produced
the right support facts once but omitted the required `handbook/plans.md` provenance. Gemini's
support miss was an HTTP 400 before generation: its native endpoint rejected surfaced operation
schemas containing arrays without `items` and required names absent from `properties`.

The Bitcoin-to-Slack smoke selected Slack on all four models and executed no write. Codex, DeepSeek,
and GPT-5-mini reached the approval boundary and honored denial. Gemini stopped before approval on
the same provider schema incompatibility, independently reproducing it for `blocks.items`; this is
tracked as [A-81](../stories/A-81-provider-native-schema-portability.md).

The optional same-provider Gemini-intent diagnostic under the DeepSeek parent passed 3/3 support
turns with a 1,922 ms median intent stage, but total support latency was 26,509 ms. It did not qualify
for five-trial confirmation and cannot justify an automatic override.

The live artifacts used while developing this evaluator are intentionally outside Git under
`/tmp/flux-a78-latency-v2-results`. Reproduce the report and keep decision with:

```bash
FIXTURE_DIR=/tmp/flux-a78-latency-v2 \
RESULTS_DIR=/tmp/flux-a78-latency-v2-results \
  scripts/eval-adaptive-latency.sh report

FIXTURE_DIR=/tmp/flux-a78-latency-v2 \
RESULTS_DIR=/tmp/flux-a78-latency-v2-results \
  scripts/eval-adaptive-latency.sh gate
```
