# Flux-Lang authoring ergonomics — typed protocols, concise data and control

**Status:** proposed · **Pillar:** Language · **Epic:**
[L-131](../stories/L-131-flux-lang-authoring-ergonomics-epic.md) · **Stories:**
[L-132](../stories/L-132-structural-record-enum-and-refinement-types.md) ·
[L-133](../stories/L-133-typed-task-result-contracts.md) ·
[L-134](../stories/L-134-recoverable-validation-and-repair.md) ·
[L-135](../stories/L-135-local-pure-functions-and-constructors.md) ·
[L-136](../stories/L-136-multiline-values-spread-and-collection-update.md) ·
[L-137](../stories/L-137-settled-and-data-driven-parallel-fanout.md) ·
[L-138](../stories/L-138-indexed-collecting-repeat-loops.md) ·
[L-139](../stories/L-139-structured-task-input-and-context.md) ·
[L-140](../stories/L-140-first-class-option-and-result-values.md)

## Why

Flux can express substantial agent workflows today, but authors pay a ceremony tax whenever a flow
has a typed protocol, a repair cycle, several similar reviewers, or a structured final result. The
language should let those ideas appear directly without moving policy into opaque Bash or a
provider-specific prompt dialect.

This is an ergonomics epic, not a new execution model. Its proposals preserve the existing safety
envelope, event model, determinism rules, and provider boundary. It follows
[L-102](../stories/L-102-flux-syntax-simplification-epic.md)'s reduction to one canonical dialect and
depends on [L-113](../stories/L-113-flux-lang-hardening-epic.md)'s parser/runtime invariants. Where a
proposal overlaps an existing story—notably L-110's value-template unification and D-05's optional
structured task output—the existing design remains authoritative and this epic supplies the next
author-facing layer.

## Problem shape

A realistic provider-neutral workflow often needs to:

1. collect evidence;
2. ask one or more roles for results with the same schema;
3. distinguish malformed output from a valid negative verdict;
4. repair only the malformed result;
5. revise a candidate from the combined findings; and
6. return one stable machine-readable result.

Today that tends to produce repeated object literals, repeated `task` calls, JSON parse/assert
boundaries, hand-maintained iteration counters, and duplicated terminal returns. The program works,
but its line count hides the protocol it is meant to communicate. Model authors pay twice: more
source must be generated, and more source must be retained as context before making the next edit.

The target is not the shortest possible syntax. It is the smallest surface that keeps types,
effects, failure, concurrency, and provenance visible.

## Design principles

- **Provider-neutral.** No construct names a model vendor, response API, or provider-specific
  schema mechanism. Providers adapt to a Flux contract at the runtime seam.
- **Domain-neutral.** The primitives work for release checks, migrations, incident triage, content
  review, and other workflows. Domain policy stays in user programs.
- **Static where possible, explicit at runtime.** The analyzer rejects contradictions it can prove;
  runtime validation produces typed data when external values do not satisfy a contract.
- **No hidden effects.** Pure functions stay pure. Fan-out, retry, repair, and optional effects are
  visible in source and trace events.
- **One canonical spelling.** Each child story coordinates with L-102 before adding syntax and must
  update every maintained grammar mirror and generated artifact.
- **Bounded recovery.** Repair and repetition always have explicit finite limits.
- **Lower to existing semantics first.** Syntactic sugar should lower to existing AST/runtime
  behavior where that preserves trace fidelity and avoids parallel implementations.

## Illustrative end state

The following sketches are deliberately generic. They are a north star, not an accepted grammar;
each child story owns its exact spelling and may reject syntax that does not compose with the
canonical dialect.

```flux
enum Verdict = "clear" | "revise"

record Finding {
  severity: "info" | "warning" | "error"
  location?: String
  problem: String
  suggestion: String
  evidence: List<String>
}

record Review<S> {
  stage: S
  verdict: Verdict
  findings: List<Finding>
}

record RunResult {
  status: "ok" | "blocked"
  candidate: String
  reviews: List<Review<String>>
  failure_kind?: String
}

fn blocked(kind: String, candidate: String, reviews: List<Review<String>>) -> RunResult
  return RunResult {
    status: "blocked"
    candidate
    reviews
    failure_kind: kind
  }

request = input.request
evidence = collect_evidence(request)
candidate = task(
  role: "planner"
  input: { request, evidence }
  context: [file("POLICY.md")]
) as String

checks = [
  { stage: "correctness", role: "review-correctness" },
  { stage: "safety", role: "review-safety" },
  { stage: "clarity", role: "review-clarity" }
]

repeat 3 as iteration, until: cycle.clear -> history
  when previous?
    candidate = task(
      role: "reviser"
      input: { candidate, findings: previous.findings }
    ) as String

  parallel settled each check in checks -> reviews
    task(
      role: check.role
      input: { candidate, stage: check.stage }
    ) as Review<check.stage>
      repair 1 with validation_error

  cycle = {
    iteration
    reviews
    findings: reviews.flat_map(.findings)
    clear: reviews.all(.verdict == "clear")
  }
  yield cycle

when history.last?.clear
  return RunResult { status: "ok", candidate, reviews: history.last.reviews }
else
  return blocked("review_limit", candidate, history.last?.reviews ?? [])
```

This sketch compresses the workflow because the source gains reusable concepts, not because it
hides work. A reader can still see the roles, inputs, context, output contract, repair bound,
parallel boundary, iteration bound, findings, and terminal state.

## Proposed language capabilities

### 1. Structural records, enums, and refinements (L-132)

Programs need local names for recurring data shapes and finite protocol vocabularies. The minimum
useful facility covers record fields, optional fields, lists, literal unions or enums, nesting, and
use in annotations. Refinements should start with constraints Flux can validate deterministically;
arbitrary user code inside a type is out of scope.

```flux
enum State = "pending" | "ready" | "failed"
record Check { state: State, messages: List<String>, score: Number where 0 <= self <= 1 }
```

### 2. Typed role/task result contracts (L-133)

A task boundary should be able to name the Flux type it must return. The provider adapter may use a
native structured-output facility when available, but the program observes one contract: a
validated Flux value or a typed validation failure. The trace records the requested contract and
validation outcome without leaking provider-specific payloads.

```flux
assessment = task(role: "assessor", input: { artifact }) as Check
```

This extends rather than replaces the optional structured output described in
[sub-agent hardening](sub-agent-hardening.md): D-05 establishes the runtime seam; L-133 makes the
contract a language-level value shared by the analyzer, runtime, and tooling.

### 3. Recoverable validation and bounded repair (L-134)

`assert` is appropriate for a violated invariant and should remain fatal. External structured data
needs a recoverable validation path: preserve the validation diagnostics, optionally make a bounded
repair attempt, then branch or return explicitly.

```flux
assessment = task(role: "assessor", input: { artifact }) as Check
  repair 1 with validation_error
  else return blocked("invalid_assessment", validation_error)
```

### 4. Local pure functions and constructors (L-135)

Repeated result construction should have a name. Local functions must be pure, non-recursive in the
first version, statically analyzable, and incapable of disguising operations or approvals.

```flux
fn failure(kind: String, messages: List<String>) -> Outcome
  return Outcome { status: "failed", kind, messages }
```

### 5. Multiline values, spread, and collection update (L-136)

Large values need an indentation-friendly spelling, while lists and records need composable spread
and append/update operations. This work follows L-110 so it does not introduce another literal vs
template distinction.

```flux
combined = {
  ...base
  state: "ready"
  messages: [...lint.messages, ...tests.messages]
}
history += combined
```

### 6. Settled and data-driven parallel fan-out (L-137)

Authors should be able to map a homogeneous operation over data concurrently and receive every
outcome. `settled` means one failed branch becomes data rather than cancelling siblings; it does not
mean ignored failure. Ordering, limits, cancellation, and trace identity must be specified.

```flux
parallel settled each target in targets, limit: 4 -> attempts
  deploy(target) as Deployment
```

### 7. Indexed, collecting repeat loops (L-138)

Bounded refinement cycles need a visible index, the prior yielded value, and a collected history.
The feature should extend the hardened `repeat` semantics from L-116 rather than create another
loop engine.

```flux
repeat 3 as iteration, until: result.ready -> history
  result = improve(candidate, previous?)
  yield { iteration, result }
```

### 8. Structured task input and context references (L-139)

Prompt interpolation is a weak transport for typed values and provenance. Tasks should accept a
structured input value and explicit context references while retaining `task: String` for prose-led
uses and compatibility.

```flux
plan = task(
  role: "planner"
  input: { request, constraints }
  context: [file("POLICY.md"), value(prior_decision)]
) as Plan
```

### 9. First-class optional and result values (L-140)

Lenient reads and caught failures need a type and exhaustive control flow rather than sentinel
strings or assertion tricks. Optional access and recoverable operations should converge on
`Option<T>` / `Result<T, E>` semantics with one canonical matching form.

```flux
config = read?("config.json")
match config
  some value
    use(value)
  none
    use(default_config)
```

## Boundaries and non-goals

- No blog-, review-, deployment-, or incident-specific keywords.
- No provider-specific schema declarations or prompt wrappers.
- No implicit unbounded retry, fan-out, recursion, or repair.
- No exception mechanism that bypasses the event log or approval envelope.
- No user-defined effectful functions in this epic; operations remain declared operations.
- No second syntax for capabilities already being canonicalized by L-102.
- No requirement that every provider support native structured output. Validation at the Flux
  boundary is the portable baseline.

## Sequencing

1. L-132 establishes reusable type vocabulary.
2. L-133 consumes those types at task boundaries; L-134 adds bounded recovery from validation
   failure.
3. L-135 and L-136 remove repeated data-construction ceremony.
4. L-137 and L-138 compress repeated control flow after L-116's loop semantics are settled.
5. L-139 makes task inputs and context structural.
6. L-140 unifies optional and recoverable outcomes across the surface.

Some implementation may proceed in parallel after the exact AST and lowering seams are recorded,
but the epic closes only when the combined example can be expressed without provider-specific
syntax and without weakening any safety invariant.

## Cross-cutting acceptance bar

Every child story must:

- start with a decision-complete syntax/AST/lowering note when the exact shape is not already fixed;
- add a failing-first parser, formatter, analyzer, and runtime test for each affected layer;
- preserve parse/format/parse equivalence and regenerate checked-in goldens;
- update the LSP, tree-sitter, TextMate, and IntelliJ mirrors when its syntax changes;
- add at least one provider-neutral, domain-neutral example to the public language examples or docs;
- document trace and serialization behavior, including compatibility for old AST/event data; and
- demonstrate that approval, confinement, cancellation, and execution budgets are unchanged or
  strengthened.

## Success measure

Use a generic three-check, three-cycle refinement flow as the comparison fixture. The new program
must preserve observable behavior while materially reducing repeated task/parse/assert/return
scaffolding. Line count is supporting evidence, not the goal; the primary test is whether the
protocol is visible from the program structure without consulting provider prompts or host code.
