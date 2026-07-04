---
id: L-37
title: Multi-perspective example — parallel 3-lens scout fan-out, merged and synthesized to a cited Answer
pillar: Language
status: done
epic:
design:
note: every capability already ships (parallel branch binds, task→.flux/agents roles, merge/synth/observe, prelude Answer) — the work is authoring the example in the REAL indentation grammar (the sketched brace/call()/let/@param syntax is not the language) + 3 scout role files + a strict_review.rs-style hermetic test
---

# Multi-perspective example — parallel 3-lens scout fan-out, merged and synthesized to a cited Answer

## Goal
Ship a second checked-in, test-guarded native-text showcase (after `examples/strict_review.flux`):
`examples/multi-perspective.flux` runs one query through three sub-agent lenses in `parallel`
(tech / product / risk scouts resolved from `.flux/agents/*.md` role files via the `task` op),
extracts each scout's `.evidence`, `merge`s the claim lists, and `synth`esizes a cited prelude
`Answer` — demonstrating in one small flow that fan-out orchestration, role-file sub-agents, and
the cognition ops compose in the language, not in host code.

## Acceptance
- [x] `examples/multi-perspective.flux` is checked in, written in the **real indentation grammar**
      (`flow multi-perspective(query: String) -> Answer`, bare `parallel` + `branch $name` arms,
      direct `op({...})` calls, `$x =` binds, `.evidence` field-access sugar — see Notes for the
      grounded sketch). No brace bodies, no `let`, no `@param`, no `call("op", {...})` — those are
      not the language (`crates/flux-lang/src/parse.rs`), and `examples/advanced-code-review.flux`'s
      brace style must NOT be imitated (it is aspirational, not parsed).
- [x] `.flux/agents/tech-scout.md`, `.flux/agents/product-scout.md`, `.flux/agents/risk-scout.md`
      are checked in with only real frontmatter fields (`name`/`description`/`model`/`tools`,
      `crates/flux-agent/src/role.rs` `RoleFrontmatter`) and `tools:` lists naming only registered
      ops (verified real: `read`, `grep`, `glob`, `web_search`, `file_stat`, `cargo_check`,
      `cargo_test`).
- [x] Failing-first hermetic integration test `crates/flux-sdk/tests/multi_perspective.rs`, modeled
      on `crates/flux-sdk/tests/strict_review.rs` (FlowClient + `RoleRegistry` over the REAL
      checked-in role files + mock provider returning a canned scout `Answer` JSON per role system
      prompt; no API key). Asserts: all three branches bind their scout result; the merged claim
      list contains each scout's evidence; the flow's return value conforms to the prelude `Answer`
      shape (the declared `-> Answer` is NOT enforced by the analyzer — the test must check it);
      each scout role is spawned exactly once.
- [x] The mock/test wiring covers `synth` too — it is provider-injected via the `CognitionPack`
      (`crates/flux-cognition/src/lib.rs`), not a static tool group, so the hermetic test must
      either drive it with the same canned provider or wire the pack explicitly.
- [x] Workspace gate green: `cargo test`, `clippy -D warnings`, and `cargo fmt --check` in BOTH
      workspaces (root + plugins/). (2026-07-04 orchestrator pass: full root + plugins/ gate ran
      green — clippy clean, all tests 0 failed, fmt clean in both workspaces.)

## Progress
- 2026-07-04 — filed. Grounding pass done against the live tree (see Notes): all five proposed ops
  map to shipped capability; the only work is surface authoring + role files + the test. No
  language, op, or orchestration code changes required.
- 2026-07-04 — in-progress. Second grounding pass surfaced two test-critical gotchas beyond the
  filed notes: (1) the hermetic test CANNOT use strict_review's panicking top-level provider —
  `synth` is a CognitionPack op that calls the provider passed to `FlowClient::build`, so one mock
  must serve both the top-level (synth) and the sub-agent factory, disambiguated on
  `req.system_text()`; (2) synth's `run_model` collects ONLY `Chunk::TextDelta` while sub-agent
  task results are read from `Chunk::Block(Text)` — the mock must emit both chunk shapes.
- 2026-07-04 — implemented, gate green. Landed all three deliverables:
  - `examples/multi-perspective.flux` — the grounded sketch verbatim (flow name kept **hyphenated**,
    `multi-perspective`, matching the acceptance's literal header text — see "flow-name hyphen"
    finding below).
  - `.flux/agents/{tech,product,risk}-scout.md` — frontmatter `description`/`tools` only (`name`
    omitted, defaults from the filename stem; no `model` override). `tools:` lists per the story:
    tech-scout `[read, grep, glob, file_stat, cargo_check, cargo_test]`, product-scout
    `[read, grep, glob, web_search]`, risk-scout `[read, grep, glob, file_stat, web_search]`. Each
    body instructs a first-message-only JSON reply shaped like the prelude `Answer`
    (`status`/`summary`/`evidence`/`gaps`/`risks`, `evidence` entries `{"claim": {"text", "confidence"}}`
    per `crates/flux-lang/src/prelude.rs`'s `Answer`/`Evidence`/`Claim` shapes) and contains the
    distinctive matchable phrase ("TECHNICAL scout" / "PRODUCT scout" / "RISK scout").
  - `crates/flux-sdk/tests/multi_perspective.rs` — two tests: `multi_perspective_fans_out_merges_and_synthesizes_a_cited_answer`
    (exactly 3 `task` calls; each scout phrase recorded exactly once; the `synth` request's prompt
    carries all three scout markers — proving branch-bind → `.evidence` → `merge` composed
    end-to-end; `out.answer()` parses with non-empty `summary`/`evidence`) and
    `multi_perspective_is_stable_across_repeated_runs` (mirrors strict_review's determinism check).
    One `MultiPerspectiveMockProvider` (shared `Arc<Mutex<Vec<String>>>` request log) served as both
    the top-level provider (`synth`) and, via the sub-agent factory closure, each scout — disambiguated
    on `req.system_text()`.
  - `.gitignore` touched (one line): added `!.flux/agents/*-scout.md` alongside the existing
    `!.flux/agents/review-*.md` negation — the three new role files would otherwise be silently
    gitignored by the blanket `.flux/agents/*` rule and never actually check in, failing the
    acceptance's "checked in" requirement.

  **Findings / deviations from the task brief:**
  - **Flow-name hyphen resolved, no deviation needed.** Verified empirically (a scratch
    `flux-lang`-only binary parsing `flow multi-perspective(...)`) AND by reading the grammar: the
    header's name charset is `is_name_char` (`parse.rs:752`), which explicitly includes `-`, and
    `analyze.rs`'s `is_valid_decl_name`/diagnostic text ("flow names contain only ASCII letters,
    digits, `_`, or `-`") confirms flow names are NOT restricted to identifier chars the way bind
    targets are. So the flow keeps the acceptance's literal `multi-perspective` name — no underscore
    substitution was needed (an earlier working assumption in this task's brief was wrong on this
    point).
  - **Chunk-shape gotcha resolved by NOT double-emitting.** Traced the two consumers precisely
    instead of guessing: `synth`'s `run_model` (`flux-cognition/src/lib.rs:314-330`) reads only
    `Chunk::TextDelta`, full stop — `Chunk::Block` is never inspected there. The sub-agent path
    (`flux-flow/src/compile.rs`'s `stream_blocks`/`compile_turn_inner`) collects both `blocks` and
    `acc_text`, but when `blocks` is non-empty the accumulated `TextDelta` text is discarded, not
    appended (`compile.rs:585-587`) — so emitting `Chunk::Block(ContentBlock::Text)` alone (matching
    `strict_review.rs`'s already-proven reviewer mock) is both correct and sufficient for scouts;
    only the `synth` branch of the mock needs `Chunk::TextDelta`. No double-emission was needed or
    used.
  - Child sub-agent base registry: `ToolRegistry::new()` (empty), same as strict_review — confirmed
    via `ToolRegistry::subset` (`flux-runtime/src/lib.rs:416`) that a role's `tools:` allowlist
    naming ops absent from an empty base is a silent no-op filter, never an error; the mock scouts
    never call a tool anyway (first-message JSON reply).

  **Gate (package-scoped, this story):**
  - `cargo test -p flux-sdk --test multi_perspective` → 2 passed.
  - `cargo test -p flux-sdk` (full crate, regression check) → 18 + 2 + 2 + 19 + 2 unit/integration
    tests passed, 0 failed, 1 doc-test passed (2 ignored, pre-existing).
  - `cargo clippy -p flux-sdk --all-targets -- -D warnings` → clean.
  - `cargo fmt -p flux-sdk -- --check` → clean (after one `cargo fmt` pass reformatted the new test
    file's line-wrapping).
  - Failing-first verified explicitly: moved `examples/multi-perspective.flux` and the three role
    files aside, reran the test (both cases failed on the `RoleRegistry::load` assertions, the right
    reason), then restored and reran green.
  - Full-workspace gate (`cargo test`/`clippy`/`fmt --check` across both the root and `plugins/`
    workspaces) was NOT run by this story — left to the orchestrator per the task's scoping.

## Notes

### Grounding: proposed sketch → what the language actually is
The story originated from a brace-syntax sketch. Verified against the parser
(`crates/flux-lang/src/parse.rs`) and op registries — the deltas:

| Sketch | Reality |
|---|---|
| `flow multi-perspective -> Answer` + `@param query: string` | params live in the header parens: `flow multi-perspective(query: String) -> Answer` (`parse_header`/`parse_params`). No `@param`. Lowercase `string` silently becomes `Named("string")` — use `String`. |
| `parallel { "technical" => { ... } }` | bare `parallel` keyword, indented `branch $technical` arms; each arm is a multi-statement block whose **trailing statement's value binds to the branch name** (`runtime.rs` `bind_existing`); `return` is forbidden inside a branch. See `examples/strict_review.flux:37`. |
| `let r = call("task", {...})` | `$r = task({ role: "tech-scout", task: $query })` — no `let`; no `call(...)` meta-op (it would parse as an unregistered op literally named `call`); a lone object argument is the named input map. |
| `call("jq", {path: ".evidence", input: $technical})` | field-access sugar: `$claims_tech = $technical.evidence` lowers to the `Jq` node. No registered `jq` op exists. |
| `merge`, `synth`, `observe` | exact matches: `merge({lists})` (`flux-tools/src/cognition.rs`, always-on cognition group), `synth({claims, cite, format}) -> Answer` (`flux-cognition/src/lib.rs`, CognitionPack), `observe({kind, data})` (`flux-tools/src/evidence.rs`, always advertised). |
| `.flux/agents/*.md` role files | exactly the shipped mechanism: `RoleRegistry::load` (`flux-agent/src/role.rs`) reads `*.md` (frontmatter `name/description/model/tools`, body = system prompt); the `task` op resolves `role` through it (`flux-orchestrate/src/lib.rs` `LocalSpawner::spawn`). CLI loads `cwd/.flux/agents` + `~/.flux/agents`. |

### Grounded sketch (the real grammar)
```
flow multi-perspective(query: String) -> Answer
  parallel
    branch $technical
      observe({ kind: "lens-start", data: { perspective: "technical" } })
      $technical = task({ role: "tech-scout", task: $query })
    branch $product
      observe({ kind: "lens-start", data: { perspective: "product" } })
      $product = task({ role: "product-scout", task: $query })
    branch $risk
      observe({ kind: "lens-start", data: { perspective: "risk" } })
      $risk = task({ role: "risk-scout", task: $query })

  observe({ kind: "lens-end", data: { perspectives: ["technical", "product", "risk"] } })

  $claims_tech = $technical.evidence
  $claims_prod = $product.evidence
  $claims_risk = $risk.evidence
  $all_claims = merge({ lists: [$claims_tech, $claims_prod, $claims_risk] })

  $answer = synth({ claims: $all_claims, cite: true, format: "detailed" })
  observe({ kind: "synthesis", data: $answer })
  return $answer
```
Branch-tail gotcha: the branch's **last** statement's value is what binds to the branch name, so
the `task(...)` bind must come last in each arm — a per-branch trailing `lens-end` observe would
overwrite the scout result with the observe ack. Sketch moves `lens-end` after the join (one
observation for all three); if per-lens end markers matter, verify at implementation time whether a
bare trailing `$r` expression-statement parses and re-order accordingly.

### Role files
The sketched frontmatter+body files ship near-verbatim (fields match `RoleFrontmatter`; all listed
tool names are registered ops). `Role::to_spec` subsets the registry to the `tools:` list, so the
scouts get real read/search capability while staying capability-scoped. Body prompts should
instruct scouts to answer with the `Answer` shape (`summary`/`evidence`/`gaps`/`risks`) — mirror
the JSON-contract discipline of `.flux/agents/review-security.md`.

### Out of scope (deliberately)
- Verbatim-sketch language features: `@param`, `let`, brace-delimited bodies, `"label" => {}` arms,
  a `call(op, args)` meta-op, a registered `jq` op. None are needed; file a separate language story
  if that surface is ever actually wanted.
- Return-type enforcement: `-> Answer` is carried as metadata only (`analyze.rs` copies `returns`;
  nothing checks the returned value). Known residual, not this story.
- `agent` decls (native-text `AgentDecl`, consumed by flux-app) are a DIFFERENT mechanism from
  spawner roles — no bridge exists and this story must not conflate them; role files are the right
  vehicle here.
- Live-provider smoke (`flux flow run examples/multi-perspective.flux`) — worth a manual run once
  landed, but the acceptance gate is the hermetic test.
