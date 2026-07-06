# Design: plan corpus + small-model spike

**Moved** — this initiative's design (canonical plan text → NL→`.flux` corpus mining → small-model
fine-tune) is flux-model intent and is tracked in the **flux-model repo**:
`../flux-model/docs/designs/plan-corpus-and-small-model.md` (stories: `../flux-model/docs/stories/`,
M-01..M-16). The flux-side pieces it spawned are regular flux stories and stay on this board:
[L-38](../stories/L-38-canonical-plan-source.md) (done, `plan_source` on `PlanAttempted`),
[L-39](../stories/L-39-multiline-strings.md) (done), [D-53](../stories/D-53-plan-source-exporter.md)
(done, `flux corpus export`), [L-40](../stories/L-40-emission-ab-finetuned-arm.md) (backlog). The
L-20 emission-A/B decision record stays in flux: [flux-lang-emission-ab.md](flux-lang-emission-ab.md).
