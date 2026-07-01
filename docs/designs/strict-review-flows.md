# Design: strict review flows and journeys

**Status:** landed (epic; all four phases built) · **Pillar:** Language (Flux-Lang protocol) + Core
(capability enforcement) · **Stories:** [L-10](../stories/L-10-strict-review-example-flow.md) ·
[L-11](../stories/L-11-strict-review-scoped-capabilities.md) ·
[L-12](../stories/L-12-strict-review-typed-artifacts.md) ·
[L-13](../stories/L-13-strict-review-journey-cli.md)

## Why

A skill can only advise a reviewer; a code-review protocol needs enforceable guarantees — fixed
step order, a bounded tool set per phase, sub-agents on a frozen context instead of ambient
workspace authority, and deterministic aggregation. This epic expresses **strict code review as an
enforced Flux-Lang flow**, not prompt convention, matching the project invariant that *the LLM is
not the runtime*: prompt guidance may inspire the protocol, but the executable flow and runtime
policy enforce it. See the detailed problem statement and phased approach below.

## Problem

A skill is a good way to give the model review guidance, but it is intentionally advisory. A code-review protocol needs stronger guarantees:

- the order of work is fixed and auditable;
- each step has a bounded tool set;
- sub-agents receive a frozen context instead of ambient workspace authority;
- aggregation is schema-driven and deterministic where possible;
- any model choice is constrained to declared branches or structured output.

Without a first-class design, a "review" workflow tends to collapse back into prompt convention: ask a reviewer to inspect files, hope it uses the right tools, then ask another model to summarize. That conflicts with flux's central invariant: the LLM is not the runtime.

## Goals

- Express a reusable review protocol as Flux-Lang, not as prompt-only instructions.
- Allow the same protocol to be exposed as a `flux-app` journey.
- Enforce different tool capabilities for different phases.
- Support bounded parallel sub-agent review over a frozen context pack.
- Produce a structured `ReviewReport` artifact suitable for CLI, TUI, CI, and future evals.
- Keep all filesystem, process, network, and model effects routed through the existing runtime safety envelope.

## Non-goals

- Replace free-form ad hoc agent review.
- Add a second policy engine or bypass `Executor::dispatch`.
- Make model prose fully deterministic.
- Grant sub-agents hidden ambient access to the parent workspace.
- Solve human review assignment, Git hosting comments, or approval workflows in the first slice.

## Vocabulary

- **Skill**: advisory model context. It can teach a reviewer how to behave, but it does not enforce behavior.
- **Flow**: executable Flux-Lang protocol. It owns step order, data dependencies, effects, budgets, and branching.
- **Journey**: app-level entrypoint that triggers a flow from an event, command, or channel.
- **Role**: constrained agent persona/tool selection used by a flow.
- **Capability scope**: a runtime-enforced set of tools/effects available to one flow block or sub-agent invocation.

## User story

A user wants to run:

```text
flux review --files crates/foo/src/lib.rs crates/bar/src/main.rs
```

or an app journey:

```flux
journey review_code(input) {
  run strict_review(files: input.files, diff: input.diff)
}
```

The workflow should:

1. read exactly the requested context with read-only tools;
2. launch multiple specialized reviewers with no filesystem tools by default;
3. collect typed findings;
4. deduplicate, rank, and synthesize the final report;
5. fail closed if any step asks for an undeclared tool.

## Proposed model

Represent strict review as a normal Flux-Lang flow plus two small language/runtime extensions:

1. `with_tools` / scoped capabilities for blocks.
2. capability-restricted sub-agent invocation.

Conceptual syntax:

```flux
flow strict_review(files: List<String>) -> ReviewReport {
  with_tools ["git_status", "git_diff", "read_many", "ctx"] {
    status = git_status()
    diff = git_diff()
    sources = read_many(files)
    review_ctx = ctx(
      include: [status, diff, sources],
      budget: 40000,
      purpose: "strict code review"
    )
  }

  with_tools ["task"] {
    parallel {
      security = task(
        role: "security-reviewer",
        tools: [],
        task: ReviewRequest { context: review_ctx, focus: "security" }
      )
      correctness = task(
        role: "correctness-reviewer",
        tools: [],
        task: ReviewRequest { context: review_ctx, focus: "correctness" }
      )
      maintainability = task(
        role: "maintainability-reviewer",
        tools: [],
        task: ReviewRequest { context: review_ctx, focus: "maintainability" }
      )
    }
  }

  with_tools ["review.normalize", "dedupe", "sort", "review.summarize"] {
    findings = review.normalize([security, correctness, maintainability])
    unique = dedupe(findings, by: "fingerprint")
    ranked = sort(unique, by: "rank", order: "desc")
    return review.summarize(ranked)
  }
}
```

The concrete AST can initially lower `with_tools` to a new block node such as `cap_scope`, or to metadata on existing `seq`, `parallel`, and `each` nodes. The important property is analyzer-visible and runtime-enforced capability narrowing.

## Review artifacts

**Built (L-12):** `ReviewFinding` and `ReviewReport` are `schemars`-derived Rust structs in
`crates/flux-tools/src/cognition.rs` (embedded schema via `flux_spec::tool_input_schema`/
`serde_json` derive — not hand-written JSON), matching the shape below with one deliberate
simplification: no `id`/`span` fields (unused by the current reviewers; add if a future surface
needs them), and `agreement: u32` was added (not in the original sketch) to carry the "reviewer
disagreement merged into a count" decision from Open questions. No `ReviewRequest` struct exists —
reviewer input is just the role's task prompt string built via `fmt` in the flow, so there was no
aggregator-facing shape to schema; add one if a future caller needs a typed request. Promotion to
`flux_lang::prelude::PRELUDE_TYPES` is deferred until a second surface consumes these types (tracked
in L-12's story notes, not yet started).

```rust
ReviewFinding {
  fingerprint: String,       // computed by review.normalize/aggregate — never model-supplied
  severity: "critical" | "high" | "medium" | "low" | "info",
  category: String,
  file: String?,
  line: Number?,
  title: String,
  evidence: String,
  recommendation: String,
  confidence: Number,
  reviewer: String,
  agreement: Number,         // distinct reviewers that raised this fingerprint (>=1)
}

ReviewReport {
  summary: String,
  findings: List<ReviewFinding>,  // deduped + ranked
  checked_files: List<String>,
  reviewers: List<String>,
  gaps: List<String>,             // quarantined malformed reviewer entries, human-readable
}
```

## Capability scoping

Capabilities should be narrowed, never widened, as execution descends:

```text
session policy
  ∩ AgentSpec tool selection
  ∩ flow-declared tools/effects
  ∩ block capability scope
  ∩ sub-agent invocation scope
```

If a block only allows `read_many`, a call to `grep` fails even if the outer session policy allows `grep`. If a sub-agent is invoked with `tools: []`, it can reason over its supplied context but cannot read more files, run shell commands, or call network tools except the provider/model call required for the role itself.

This must be enforced in the runtime dispatch path, not by prompt text. A denied call should produce a normal policy/capability error and be visible in the evidence log.

**Built (L-11):** the `flow-declared tools/effects ∩ block capability scope` step is `with_tools [...] { … }`
(`Node::CapScope`), enforced by `Executor::dispatch`'s capability-scope stack — see Phase 2 below for
the exact locus. The `∩ sub-agent invocation scope` step is `Spawner::spawn_scoped` intersecting the
role's `tools` with the caller's active block scope; `task` does **not** carry its own `tools:`
parameter (that idea is superseded — see Open questions).

## Sub-agent behavior

Sub-agents should be treated as effectful model calls with explicit inputs and tool caps:

- Parent flow builds a `Ctx` pack.
- Parent flow invokes a named `Role` through `task` or a future typed `agent.review` op.
- The invocation includes an explicit tool allowlist.
- The child engine is assembled from the role, then intersected with the invocation allowlist.
- The child receives the context pack and output schema.
- The child returns JSON findings, not an unstructured essay.

The strict default for review should be no child tools. If a reviewer needs inspection tools later, add them intentionally per role or per invocation, for example `tools: ["grep"]` for a dependency-focused reviewer.

## Aggregation

Aggregation should be deterministic by default:

1. Parse each reviewer output into `ReviewFinding[]`.
2. Reject or quarantine malformed findings.
3. Generate a stable fingerprint from category, file, line/span, and normalized title.
4. Deduplicate by fingerprint.
5. Rank by severity, confidence, and reviewer agreement.
6. Produce a report with stable ordering.

A model may be used for final prose synthesis, but only after deterministic aggregation and with a fixed schema. The model should not decide which extra tools to run or which reviewers to spawn.

**Built (L-12):** two native Rust ops in `crates/flux-tools/src/cognition.rs` (the `cognition` group,
force-on like `dedupe`/`sort`), not flow-level composite ops — fingerprinting needs a stable hash
that composite `dedupe`/`sort` over a model-supplied field cannot provide deterministically (a model
`rank`/`fingerprint` is exactly what L-10 shipped and L-12 replaces).

- `review.normalize({ findings: Value[] })` → `{ findings: ReviewFinding[], gaps: String[] }`. Each
  raw entry is parsed (step 1); an entry that is not an object, or is missing `title`/`category`, or
  has a missing/unrecognized `severity`, becomes a `"dropped malformed finding: …"` gap string instead
  of an error or a silent drop (step 2). A well-formed entry's fingerprint is
  `hash(category + "\u{1}" + file + "\u{1}" + line + "\u{1}" + normalize(title))` via a fixed-key
  `DefaultHasher` (step 3) — deterministic across processes and runs because the key is fixed (not
  `RandomState`'s per-process seed) and the input string uses a separator byte (`\u{1}`) that cannot
  appear in any field, so distinct inputs cannot collide by concatenation.
- `review.aggregate({ findings, files, reviewers })` → a full `ReviewReport`. Runs `review.normalize`
  internally, then dedupes by fingerprint via a `BTreeMap` keyed on the fingerprint string (step 4;
  order-independent and hashmap-iteration-free), counting **distinct non-empty `reviewer` values** in
  each group as `agreement` and keeping the group's max `confidence`. Ranks (step 5) severity desc →
  confidence desc → agreement desc → **fingerprint asc** as a final stable tiebreak, so two findings
  can never compare equal and ordering is byte-identical run over run regardless of input order (step
  6). `summary` is a generated one-line count string; `checked_files`/`reviewers` are echoed from the
  input.

The migrated `examples/strict_review.flux` calls only `review.aggregate` for its whole aggregation
tail; the model still decides nothing about ranking, dedup, or reviewer selection — those three
reviewer roles are hardcoded in the flow, not model-chosen. Prose synthesis on top of the typed
`ReviewReport` (the "model may be used for final prose synthesis" line above) is not yet built —
tracked as Phase 4 (journey/CLI surfaces).

## Journey integration

A `flux-app` journey is the right product surface once the flow is reusable. The conceptual syntax
above is illustrative; see Phase 4 below for the exact shape that landed (a composite `op` wrapping the
checked-in flow text, not a `journey` with typed keyword args — native-text `call` args are positional,
and a journey's body reads payload-seeded symbols the same way `hello.flux`'s `echo` journey does).

The journey owns trigger and input mapping. The flow owns execution semantics. This keeps app routing separate from review correctness.

## Minimal implementation path

### Phase 1: composite review flow

- Define `strict_review` as a project/session composite op or checked-in example flow.
- Use existing `read_many`, `git_status`, `git_diff`, `ctx`, `task`, `dedupe`, and `sort` ops.
- Make reviewer prompts require JSON findings.
- Keep tool restriction for sub-agents at the `AgentSpec` / role level where possible.

This proves the shape without language changes.

### Phase 2: scoped capabilities — BUILT (L-11)

Landed as described below rather than left as an open design; see [L-11](../stories/L-11-strict-review-scoped-capabilities.md) for the acceptance mapping.

- **Language:** a new `Node::CapScope { tools: Vec<String>, body, bind }` AST node (native text
  `with_tools ["a", "b"] { … }`), not a field on the RAII `Scope` node and not a `task(tools:)` param —
  a block composes with `parallel`/any call, and a role's own `tools` still layers on top for
  sub-agents (see below). The analyzer walks it like any other block and additionally flags a
  literal-op `call` whose name is provably outside the enclosing (already-narrowed) allowlist —
  early, static feedback; the runtime gate below is still the enforcement authority.
- **Enforcement locus:** `flux-runtime`'s `Executor` grew an interior-mutable capability-scope stack
  (`ToolContext::cap_scopes: Arc<Mutex<Vec<Vec<String>>>>`, mirroring the existing `plan_scope`/
  `trust_all` pattern). `Executor::push_cap_scope(tools)` intersects `tools` with the current
  top-of-stack (narrow-only) and returns a `CapScopeGuard` whose `Drop` pops it unconditionally.
  `Executor::dispatch` checks the top of stack as its **first** gate, before pre-tool hooks and the
  policy/permission layers — so every dispatch is covered, including one reached through a composite
  op's recursive `execute_flow` or a sub-agent's own inner calls. A denial returns a normal
  `ToolResult::error` (`` `{name}` denied by capability scope ``) and records a `cap_scope_denied`
  observation; `push_cap_scope`/the guard's `Drop` record `cap_scope_enter`/`cap_scope_exit`.
- **flux-lang seam:** the `OpHost` trait (the existing seam `flux-flow`'s `ExecutorHost` already
  bridges to the real `Executor`) grew two default-no-op methods, `push_cap_scope`/`pop_cap_scope`.
  `ExecutorHost` overrides them to forward to `Executor::push_cap_scope`, holding the returned guard
  in a small internal stack (the two `OpHost` calls aren't RAII — they're separate `await` points
  around the `CapScope` node's body). The interpreter's `CapScope` handler pushes, runs its body, and
  **always** pops (mirroring `Scope`'s acquire/body/finally discipline) — the pop is unconditional even
  on an error or an early `return`. No new `flux-lang` → `flux-runtime` dependency: the language only
  knows the `OpHost` trait, same as `dispatch`/`request_approval`.
- **Sub-agent intersection:** `Spawner` grew a `spawn_scoped(role, task, cancel, cap_scope)` method
  (default delegates to `spawn`, so existing `Spawner` implementors and the unrelated
  `plan_and_dispatch` caller in `flux-eval` are unaffected). `TaskTool::execute` reads
  `ctx.active_cap_scope()` — the same shared stack `Executor::dispatch` checks — and calls
  `spawn_scoped` instead of `spawn`. `LocalSpawner` intersects the role's own `tools` with the
  incoming `cap_scope` before subsetting the child's registry, so `role.tools ∩ active_block_scope`
  bounds the child, not just `role.tools`.
- Evidence: `cap_scope_enter`/`cap_scope_denied`/`cap_scope_exit` observations, tested end to end
  (order: enter precedes any denial, denial precedes exit).
- Tests: an allowed-outer-tool-denied-inside-a-narrower-block headline test, a non-bypassable test via
  a composite op's inner call, a nesting-cannot-widen test, a no-active-scope-is-a-no-op regression,
  and a sub-agent-intersection test — all in `flux-runtime`, `flux-lang`, `flux-flow`, and
  `flux-orchestrate`.

### Phase 3: typed review artifacts and aggregator — BUILT (L-12)

Landed as native Rust ops rather than composite-ops-first, per the escape hatch this plan itself
named; see [L-12](../stories/L-12-strict-review-typed-artifacts.md) for the acceptance mapping and
the "Review artifacts" / "Aggregation" sections above for the exact shapes and algorithm.

- `ReviewFinding` and `ReviewReport` landed as `schemars`-derived embedded schemas in
  `crates/flux-tools/src/cognition.rs` (no `ReviewRequest` — not needed yet); prelude-type promotion
  is still deferred (no second surface consumes them yet).
- `review.normalize` / `review.aggregate` landed as native Rust ops directly (not composite ops first)
  — fingerprinting needs a hash that is stable independent of `HashMap`/process-random iteration
  order, which a flow-level composite over existing ops (`dedupe`/`sort`/etc.) cannot guarantee; this
  is exactly the "native Rust only if fingerprinting/ranking needs a stable built-in" condition this
  plan named, so Phase 3 skipped straight to it rather than landing an intermediate composite-op
  version.
- `examples/strict_review.flux`'s aggregation tail is now the single `review.aggregate(...)` call;
  the three reviewer roles no longer emit `fingerprint`/`rank` (computed by the aggregator).
- Tests: 3 new `flux-tools` unit tests (stable-ordering + malformed→gap, fingerprint stability +
  duplicate-collapse-with-agreement, severity→confidence→agreement ranking), plus the updated L-10
  `crates/flux-sdk/tests/strict_review.rs` integration test.

### Phase 4: app journey and surfaces — BUILT (L-13)

Landed with ONE shared `strict_review` definition structurally guaranteed (not by convention) to be
identical on both surfaces; see [L-13](../stories/L-13-strict-review-journey-cli.md) for the
acceptance mapping.

- **Shared definition:** `crates/flux-app/src/review.rs` `include_str!`s the checked-in
  `examples/strict_review.flux` (`STRICT_REVIEW_FLOW_SRC`) — the same file L-10/L-12 landed and
  `crates/flux-sdk/tests/strict_review.rs` drives directly. `strict_review_op()` parses that exact text
  once into a `DraftAst` and wraps it, unmodified, as a `strict_review` `CompositeOpDecl` (risk
  `Medium`, effects `[Read, Filesystem, Process]` — matching what `task`/`git_status`/`git_diff`/
  `read_many` actually require, or `analyze_composites` rejects the declaration with a clear
  diagnostic). There is no second, hand-maintained copy of the protocol anywhere.
- **Journey:** `review_code_journey()` is pure plumbing — `return strict_review(files: $files)` — with
  no review logic of its own; `$files` is the payload-seeded symbol (`App`'s `seed_payload`, the same
  mechanism `hello.flux`'s `echo` journey uses for `$text`). `strict_review_program()` assembles the
  full `Program` (the op + the journey + an `on "review"` trigger). `flux_app::App` grew
  `with_sub_agents` (mirrors `FlowClient::with_sub_agents`: registers `TaskTool` and installs a
  `SubAgents`-built spawner on every journey run's executor, sharing ONE `System` built once in
  `Engine::new` rather than a second independently-resolved instance) — this is what lets a journey's
  composite-op body delegate to reviewers via `task`. `flux app run strict-review` (a built-in program
  name, no file needed) runs it.
- **CLI:** `flux review --files <path>… [--format md|json] [--fail-on <severity>]`
  (`crates/flux-cli/src/main.rs`) wires roles + sub-agents via a shared `build_review_sub_agents`
  helper (exactly like `build_agent`'s construction — `load_roles` + `SubAgents::new` — and shared with
  the `flux app run strict-review` branch so the two call sites can't drift), then runs
  `STRICT_REVIEW_FLOW_SRC` through `flux_sdk::FlowClient::run_flow` (`parse` → `analyze` →
  `execute_with`, deterministic, no model round-trip for the flow itself — only the reviewer
  sub-agents call a model). Markdown (default, a readable findings + gaps summary) or raw
  `ReviewReport` JSON (`--format json`). `--fail-on <severity>` (`info|low|medium|high|critical`) is
  decided by a pure, unit-tested `should_fail(report, threshold) -> bool`; `run_review` calls
  `std::process::exit(1)` only at the top level. An unrecognized/malformed severity string maps to
  `Critical` for this gate (fail-safe — it can never silently slip under a threshold), the opposite
  convention from `flux_tools::cognition`'s ranking (which sorts an unknown severity as the *lowest*
  tier, safe for stable ordering but wrong for a security gate).
- **Self-contained:** `load_roles`'s existing `DEFAULT_ROLES` fallback pattern already covers the
  built-in review roles (a project's own `.flux/agents/review-*.md` still overrides), and the flow text
  ships in the binary via `include_str!` — so `flux review` and `flux app run strict-review` work in
  any repo, not just this one.
- **Security:** unchanged from L-11/L-12 — reviewer roles keep `tools: []`; the strict-review core
  stays read-only; the CLI only ever prints to stdout (no write/network/report-publishing effect was
  added).
- Tests: `crates/flux-app/tests/strict_review_journey.rs` (the headline test — `App::deliver("review",
  …)`'s journey result equals `FlowClient::run_flow`'s direct result, byte for byte, against the same
  mock reviewer fixture; added RED before the sub-agent wiring existed, GREEN after), `flux-app/src/
  review.rs`'s unit tests (the composite op wraps the checked-in file verbatim), and 10 new `flux-cli`
  unit tests for `should_fail` + `render_review_markdown`.

## Tests and acceptance

- A flow with `with_tools ["read_many"]` can call `read_many` and cannot call `grep`.
- A sub-agent invoked with `tools: []` cannot perform filesystem or shell operations.
- Review fan-out is bounded and deterministic in branch count.
- Aggregation produces stable ordering for the same findings.
- Malformed reviewer output is reported as a gap, not silently accepted.
- The journey path and direct flow path produce the same `ReviewReport` for the same inputs.
- Capability denials appear in the evidence log.

## Security considerations

- Capability scopes are defense-in-depth on top of policy, not a replacement for policy.
- Child agents must not inherit ambient parent tools by default.
- Context packs should record dropped members when budget trimming occurs.
- Findings must not include secrets; existing redaction still applies to tool results and evidence.
- Write/network/report-publishing actions should remain outside the strict review core and require explicit approval.

## Open questions

- ~~Should capability scopes be expressed as allowed tools, allowed effects, or both?~~ **Resolved
  (L-11): tool-NAME allowlist**, matching `ToolRegistry::subset(role.tools)` and the `with_tools
  ["git_status", …]` syntax. Effect-based narrowing is an explicit future refinement, not built here.
- ~~Should `task` grow a typed `tools` parameter, or should sub-agent capability restriction be
  represented as a surrounding block scope?~~ **Resolved (L-11): a surrounding `with_tools` block
  scope**, intersected into the role's own `tools` via `Spawner::spawn_scoped`. A block composes with
  `parallel` fan-out and covers non-`task` calls too; a `task(tools:)` param would not.
- Where should review artifact schemas live before they become prelude types?
- Should reviewer disagreement be preserved as separate findings or merged with agreement counts?
- ~~Should strict review be a built-in sample, a project template, or a first-class CLI command?~~
  **Resolved (L-13): a first-class CLI command (`flux review`) plus a built-in app journey**
  (`flux app run strict-review`), both self-contained (roles + flow text embedded in the binary) so
  neither depends on being run from within the flux repo.

## Recommendation

Start with a checked-in example flow and role files that demonstrate the protocol using existing primitives. Then add first-class scoped capabilities once the example exposes the exact runtime contract needed. This matches flux's vision: prompt guidance can inspire the protocol, but the executable flow and runtime policy enforce it.
