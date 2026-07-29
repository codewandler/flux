---
id: C-201
title: "The op reference omits schedule_wakeup, ai_segment and the eval family — and the coverage test passes anyway"
pillar: Language
status: done
priority: 13
epic: website-truth-and-identity
design: docs/designs/website-truth-and-identity.md
note: "operations_reference_covers_the_registered_public_catalog is green while three op families are missing from ops.md — the test's scope is the actual defect; also agent_loop is absent from the program declaration list"
---

# The op reference omits `schedule_wakeup`, `ai_segment` and the eval family — and the coverage test passes anyway

## Goal
`website/docs/language/ops.md` presents itself as the registered-operation reference. Three op
families are missing from every table on it. More importantly,
`operations_reference_covers_the_registered_public_catalog` in
`crates/flux-cli/tests/website_contract.rs` is **green** while they are missing — so the guard that
exists to prevent exactly this does not cover it. Fix the catalog, and fix the test that let it
drift.

## Acceptance
- [x] `schedule_wakeup` documented in its own section, cross-linked to C-200's `[wakeup]` config
      section and to `flux wakeups`, and stating that enabling the table does not grant the op.
- [x] `ai_segment` documented — under **Agent-loop stages**, which is what it actually is.
- [x] The eval family documented, in three sub-tables (run/score, mine/rank, apply-under-guard),
      under an explicit status admonition rather than as a shipping product line.
- [x] `agent_loop` added to the declaration lists in `language/modules-and-programs.md` and
      `agent/programs.md`, with a worked example and a pointer to the stage ops.
- [x] Failing-first: `operations_reference_covers_the_registered_public_catalog` **widened** (not
      duplicated) until it failed, then the catalog was filled.
- [x] The new `agent_loop` example is a complete fence and parses under the existing fence test.

## Progress
- **Diagnosed why the guard was green.** The test built its registry from four packs —
  `flux_tools::register_builtins`, `flux_web::register_web`, `CognitionPack`, `ConsultTool` — but
  `crates/flux-cli/src/execution.rs:1029-1046` assembles a real session from **four more**:
  `try_register_reflect`, `try_register_flows`, `flux_eval::try_register_eval_ops`, and the
  config-gated `WakeupTool`. Every op the audit found missing lived in exactly those four. The
  test's scope was the defect, as the story suspected.
- Widened it to match the real assembly. `schedule_wakeup` is registered **unconditionally** in the
  test even though production gates it on `[wakeup] enabled` — gated-off is still public surface a
  reader must be able to look up, which is precisely how it stayed undocumented.
- **27 ops missing, not 11.** The story listed eleven. The widened registry reported 27. Rather than
  document a flat list, I pulled each op's real `ToolSpec` (group, risk, effects, description) out
  of the registry with a temporary `#[ignore]` dump test, then removed it — so the tables carry the
  registry's own wording and risk tiers, not paraphrase.
- **The 27 split cleanly, and the split is what makes them documentable honestly:**
  - **6 in the `reflect` group** (`detect_intent`, `explore`, `approve_batch`, `execute_batch`,
    `ai_segment`, `present_results`) — never surfaced to a model catalog. These are the authored
    agent-loop stages, and they are only written by someone supplying their own `[agent] loop`. They
    are documented as such, with the approve/execute receipt split called out as the safety envelope
    made explicit.
  - **21 `<core>`** — registered in every session, so genuinely model-facing. This is the eval
    family plus its guarded git ops.
- **Two things worth stating that the audit did not surface**: `score_compare_multi` exists because
  a combined score can mask a per-benchmark regression, and `guard_protected` is the anti-cheat step
  that restores grader/suite/loop/CI paths after each worker run so a round cannot raise its own
  score by editing what measures it. Both are now in the page — they are the reason those ops exist.
- **Improvement framing.** The section carries a status admonition: the machinery is real and
  runnable, the headline grader-confirmed gain is not proven, the pillar is on hold since
  2026-07-06. Same rule C-203 applies to `agent/improvement.md`.
- **Better failure reporting.** The assertion panicked on the first omission, which is how three
  whole families accumulated one fix at a time. It now collects every omission and reports the set.
- Gate: `cargo test -p flux-cli --test website_contract` — 17 green. `npm run build` clean.

## Notes
- Diagnosing the existing test's scope is the substance of this story; the prose additions are
  mechanical once the guard is right. If the test's narrow scope turns out to be deliberate, record
  why in Progress and add the missing coverage as a sibling assertion instead.
- `crates/flux-flow/docs/ops-reference.md` is the in-repo catalog and is ahead of the website;
  reconcile toward the **registry**, not toward either document.
