# Agent reasoning effort

## Problem

`Request` and the provider codecs already support typed reasoning effort, but the agent engine drops
the setting. The CLI accepts `--effort` and `--think` as hidden no-ops, and attaching a thinking UI
sink accidentally enables adaptive thinking. Planner, completion, compaction, cognition, app-agent,
and sub-agent calls therefore disagree about one agent's reasoning policy.

## Contract

- `AgentSpec` owns `thinking: bool` and `effort: Option<Effort>`.
- Every model request made on behalf of that agent carries both values. A sink observes reasoning;
  its presence never changes model behavior.
- Markdown roles may explicitly set `thinking` or `effort`. Missing role values inherit the parent
  sub-agent configuration.
- Flux app agents accept the same keys in `settings`.
- `--think` remains an explicit adaptive-thinking switch. `--effort` remains the typed effort hint;
  neither implies the other. Provider capability profiles retain responsibility for omitting fields
  unsupported by a particular wire/model.

Existing constructors keep their current behavior by defaulting to `thinking = false` and no
effort. `FlowEngine::with_reasoning` updates both the outer engine calls and its installed reflexive
loop host, so `AgentSpec` can configure the full call graph without expanding the already-public
`FlowEngine::assemble` signature.

## Verification

Capture providers assert the setting on planner/repair, grounded completion, compaction, and
cognition requests. Role/app tests assert parsing and inheritance. CLI help and public provider docs
describe the real controls. Live low/high probes are recorded separately because one network run is
performance evidence, not a default-policy proof.

On 2026-07-13, the same Codex `gpt-5.5` one-sentence task carried the requested value all the way to
the wire and answered correctly without a planner repair in both cases:

| Effort | Input | Reasoning | First text | Provider total | Wall |
|---|---:|---:|---:|---:|---:|
| `low` | 23,283 tokens | 0 tokens | 1.488 s | 1.652 s | 2.399 s |
| `high` | 23,283 tokens | 0 tokens | 2.364 s | 2.542 s | 3.585 s |

This trivial task did not require hidden reasoning, so it proves propagation, not a quality benefit.
The latency difference is one noisy pair and does not establish a default-effort recommendation.
