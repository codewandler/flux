---
id: L-10
title: Strict review — checked-in example flow + reviewer roles (Phase 1)
pillar: Language
status: in-progress
epic: strict-review-flows
design: docs/designs/strict-review-flows.md
note: proves the strict-review protocol shape with existing primitives — no language changes
---

# Strict review — checked-in example flow + reviewer roles (Phase 1)

## Goal

Express the strict code-review protocol as a real, checked-in Flux-Lang flow (`strict_review`) plus
reviewer role files, using **only existing primitives** — the deterministic path the design's Phase 1
prescribes. Proves the shape (read-only context gather → fan-out to capped reviewers → deterministic
aggregation → structured report) without any new language/runtime feature, so the exact runtime
contract the later phases must enforce is visible in a running flow. Serves the Language pillar's
"the LLM is not the runtime" invariant: the protocol lives in an executable flow, not in prompt
convention.

Full design: [docs/designs/strict-review-flows.md](../designs/strict-review-flows.md) — Phase 1.

## Acceptance

- [ ] **Failing-first test:** an integration test drives `strict_review` over a fixed set of files
  and asserts a structured report is produced with findings from each reviewer role — added red
  (flow/roles absent), then green.
- [ ] A checked-in `strict_review` flow gathers context read-only (`git_status`/`git_diff`/`read_many`
  → `ctx` pack with a budget) and fans out to ≥2 reviewer roles via `task`.
- [ ] Reviewer roles are defined as role/AgentSpec files whose tool selection is restricted (no
  filesystem/shell tools) — the strongest restriction achievable at the role level pre-Phase-2.
- [ ] Each reviewer is instructed to return **JSON findings** (not free prose); the flow parses them.
- [ ] Aggregation is deterministic: findings are deduplicated (`dedupe` by fingerprint) and ranked
  (`sort`), producing a report with stable ordering for the same inputs.
- [ ] Fan-out is **bounded** — the branch count is fixed by the declared reviewer set, not chosen by
  the model.
- [ ] Dev loop green: `cargo build/test --workspace`, `clippy -D warnings`, `fmt`, `flux-codegate`.
- [ ] CHANGELOG entry.

## Progress
- 2026-07-01: epic seeded from the committed strict-review-flows design; grounded all Phase-1
  primitives against source (ctx/parallel-branch/task nodes, dedupe/sort/merge cognition ops, parse
  node, role tool-scoping, mock-model test harness all confirmed present). **in-progress** —
  implementation started via the story-implementer with a grounded plan.

## Notes
- Deliberately no `with_tools` node yet: sub-agent tool restriction stays at the role/`AgentSpec`
  level. The runtime-enforced narrowing is [L-11](L-11-strict-review-scoped-capabilities.md).
- Typed `ReviewRequest`/`ReviewFinding`/`ReviewReport` artifacts + a native aggregator are
  [L-12](L-12-strict-review-typed-artifacts.md); Phase 1 keeps schemas embedded in the flow.
- Uses existing ops only: `git_status`, `git_diff`, `read_many`, `ctx`, `task`, `dedupe`, `sort`.
