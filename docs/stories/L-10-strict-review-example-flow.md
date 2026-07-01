---
id: L-10
title: Strict review — checked-in example flow + reviewer roles (Phase 1)
pillar: Language
status: done
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

- [x] **Failing-first test:** an integration test drives `strict_review` over a fixed set of files
  and asserts a structured report is produced with findings from each reviewer role — added red
  (flow/roles absent), then green.
- [x] A checked-in `strict_review` flow gathers context read-only (`git_status`/`git_diff`/`read_many`
  → `ctx` pack with a budget) and fans out to ≥2 reviewer roles via `task`.
- [x] Reviewer roles are defined as role/AgentSpec files whose tool selection is restricted (no
  filesystem/shell tools) — the strongest restriction achievable at the role level pre-Phase-2.
- [x] Each reviewer is instructed to return **JSON findings** (not free prose); the flow parses them.
- [x] Aggregation is deterministic: findings are deduplicated (`dedupe` by fingerprint) and ranked
  (`sort`), producing a report with stable ordering for the same inputs.
- [x] Fan-out is **bounded** — the branch count is fixed by the declared reviewer set, not chosen by
  the model.
- [x] Dev loop green: `cargo build/test --workspace`, `clippy -D warnings`, `fmt`, `flux-codegate`.
- [x] CHANGELOG entry.

## Progress
- 2026-07-01: epic seeded from the committed strict-review-flows design; grounded all Phase-1
  primitives against source (ctx/parallel-branch/task nodes, dedupe/sort/merge cognition ops, parse
  node, role tool-scoping, mock-model test harness all confirmed present). **in-progress** —
  implementation started via the story-implementer with a grounded plan.
- 2026-07-01: **implemented and green.** Added `examples/strict_review.flux` (native-text flow:
  `git_status`/`git_diff`/`read_many` → budgeted `ctx` pack → `parallel` fan-out to 3 fixed reviewer
  roles via `task` → `merge` → `filter` (quarantine malformed entries) → `dedupe` (by `fingerprint`)
  → `sort` (by `rank` desc) → structured report) plus three restricted role files
  `.flux/agents/review-{security,correctness,maintainability}.md` (`tools: []`, JSON-only output
  contract). Added the failing-first integration test `crates/flux-sdk/tests/strict_review.rs`
  (confirmed red twice — flow absent, then roles absent — before green), driving the real checked-in
  flow + role files through `FlowClient::with_sub_agents` with a mock provider that returns a
  cross-reviewer duplicate finding and one malformed entry; asserts dedupe collapses the duplicate,
  the malformed entry never surfaces, ordering is stable across two runs, and exactly 3 `task` calls
  fire (bounded fan-out).
  - **Aggregation crux (array-from-JSON-string):** a `task` result is stored as `Value::String` (the
    sub-agent's raw text), and `merge`'s `lists` param rejects a string per element. The fix needed no
    new primitive: wrapping the three reviewer symbols in a `list` **value-template**
    (`merge({lists: [$security, $correctness, $maintainability]})`) makes `eval_template`'s per-leaf
    evaluation re-parse each `Value::String` JSON-array text into a real `serde_json::Value::Array`
    (the same `jq_parse_input` reparse a `jq`/`parse` step relies on) — confirmed by reading
    `runtime.rs`'s `eval_template`/`eval_arg`/`jq_parse_input`, not by trial and error.
  - **Two upstream bugs found and fixed while grounding the flow (both narrow, both required to make
    this exact shape work, both covered by new regression tests):**
    1. `flux-lang` analyzer (`analyze.rs::check_node`): the named-args "lone object is the whole
       input" exemption only recognized a lone `Node::Lit{object}` argument, not a lone `Node::Obj`
       **template** argument — so `task({role: "x", task: $prompt})` (an object literal with one
       dynamic field, the natural spelling for any multi-param op call with a computed value) was
       rejected as "op `task` takes 2 parameters; pass a single object argument" even though the
       runtime already treats a lone `Obj` identically to a lone `Lit` object
       (`eval_arg`/`map_args_to_input`). Fixed by extending the `lone_object` check to also match
       `[Node::Obj {..}]`. New test: `analyze::tests::lone_obj_template_argument_is_the_named_input_not_a_bare_value`.
    2. `flux-flow` planner (`compile.rs::compile_turn`): the "maybe the model emitted the AST as
       plain text" heuristic parses ANY balanced `{…}` found in prose as a `DraftAst` (whose fields
       all default, no `deny_unknown_fields`) and accepts it if `analyze_flow` succeeds — which it
       trivially does for an empty body. A reviewer's JSON-array reply (`[{"fingerprint": …,
       "reviewer": "security"}]`, exactly what this story requires reviewers to emit) contains a
       balanced finding object, so it was misdetected as an empty no-op `Plan` instead of `Chat`,
       stalling the sub-agent's turn loop until the retry-breaker force-stopped it. Fixed by requiring
       a non-empty `body` before accepting the prose-embedded-AST fallback. New test:
       `compile::tests::compile_turn_does_not_mistake_structured_json_prose_for_an_empty_plan`.
  - Gate: `cargo build --workspace`, `cargo test --workspace` (every crate green, including the new
    test + both regression tests), `cargo clippy --workspace --all-targets -- -D warnings` (clean),
    `cargo fmt --check` (clean), `cargo test -p flux-codegate` (green) — all confirmed.

## Notes
- Deliberately no `with_tools` node yet: sub-agent tool restriction stays at the role/`AgentSpec`
  level. The runtime-enforced narrowing is [L-11](L-11-strict-review-scoped-capabilities.md).
- Typed `ReviewRequest`/`ReviewFinding`/`ReviewReport` artifacts + a native aggregator are
  [L-12](L-12-strict-review-typed-artifacts.md); Phase 1 keeps schemas embedded in the flow.
- Uses existing ops only: `git_status`, `git_diff`, `read_many`, `ctx`, `task`, `dedupe`, `sort`.
