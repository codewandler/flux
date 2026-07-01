# Design: strict review flows and journeys

## Status

Proposed.

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

Add typed prelude artifacts once the flow stabilizes:

```rust
ReviewRequest {
  context: Ctx,
  focus: String,
  files: List<String>,
  schema_version: String,
}

ReviewFinding {
  id: String,
  fingerprint: String,
  severity: "critical" | "high" | "medium" | "low" | "info",
  category: String,
  file: String?,
  line: Number?,
  span: Span?,
  title: String,
  evidence: String,
  recommendation: String,
  confidence: Number,
  reviewer: String,
}

ReviewReport {
  summary: String,
  findings: List<ReviewFinding>,
  checked_files: List<String>,
  reviewers: List<String>,
  gaps: List<String>,
}
```

The first implementation can keep these as JSON schemas embedded in the review flow. Promotion to prelude types should happen when multiple surfaces consume them.

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

## Journey integration

A `flux-app` journey is the right product surface once the flow is reusable:

```flux
journey review_code(input) {
  run strict_review(
    files: input.files,
    diff: input.diff,
    reviewers: input.reviewers ?? ["security", "correctness", "maintainability"]
  )
}
```

The journey owns trigger and input mapping. The flow owns execution semantics. This keeps app routing separate from review correctness.

## Minimal implementation path

### Phase 1: composite review flow

- Define `strict_review` as a project/session composite op or checked-in example flow.
- Use existing `read_many`, `git_status`, `git_diff`, `ctx`, `task`, `dedupe`, and `sort` ops.
- Make reviewer prompts require JSON findings.
- Keep tool restriction for sub-agents at the `AgentSpec` / role level where possible.

This proves the shape without language changes.

### Phase 2: scoped capabilities

- Add an analyzer-visible capability-scope node or metadata on block nodes.
- Thread the narrowed tool/effect set through `flux-flow` into `Executor::dispatch`.
- Emit evidence for capability entry/exit and denials.
- Add tests that an allowed outer tool is denied inside a narrower block.

### Phase 3: typed review artifacts and aggregator

- Add `ReviewRequest`, `ReviewFinding`, and `ReviewReport` as schemas or prelude types.
- Implement `review.normalize` / `review.aggregate` as deterministic composite ops first.
- Promote to native Rust only if schema validation, fingerprinting, or ranking needs a stable built-in.

### Phase 4: app journey and surfaces

- Add a `flux-app` example journey.
- Optionally expose a CLI convenience command that invokes the flow.
- Add CI-friendly output modes: markdown, JSON, and nonzero exit on high severity.

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

- Should capability scopes be expressed as allowed tools, allowed effects, or both?
- Should `task` grow a typed `tools` parameter, or should sub-agent capability restriction be represented as a surrounding block scope?
- Where should review artifact schemas live before they become prelude types?
- Should reviewer disagreement be preserved as separate findings or merged with agreement counts?
- Should strict review be a built-in sample, a project template, or a first-class CLI command?

## Recommendation

Start with a checked-in example flow and role files that demonstrate the protocol using existing primitives. Then add first-class scoped capabilities once the example exposes the exact runtime contract needed. This matches flux's vision: prompt guidance can inspire the protocol, but the executable flow and runtime policy enforce it.
