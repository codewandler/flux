---
title: Types & effects
description: Flux-Lang's lightweight type surface — built-in types, effect tags that drive risk and approval, and the prelude artifact types.
---

# Types & effects

Flux-Lang has lightweight structural annotations. They document intent, feed analyzer checks, and stay
visible in the AST. They are optional today; runtime values are still JSON-like values owned by the
value store.

## Built-in types

| Syntax | Meaning |
|---|---|
| `String` | UTF-8 text |
| `Number` | 64-bit float |
| `Bool` | boolean |
| `Any` | top type — matches anything |
| `List<T>` | homogeneous list |
| `Ticket`, `Ctx`, … | named / registered types |

In the JSON wire form these correspond to the `TypeRef` tags `any`, `bool`, `number`,
`string`, `list`, and `named(X)`.

## Where types appear

```flux
flow build-report(repo: String, branch: String) -> TestResult
  $tests: TestResult = cargo_test({args: ["--workspace"]})
  return $tests
```

- **Flow parameters** — `name: Type` in the header.
- **Return types** — `-> Type` on the header.
- **Typed binds** — `$x: Type = …` in the body.

Named types come from the registered prelude (below) or host-registered schemas; a flow
references them by name.

## Effects

`FlowEffect` is the semantic effect declared on a bind — it drives risk scoring and approval
decisions in the safety envelope. Declare one with an `@effect(tag)` annotation on the line
before the bind:

```flux
@effect(send_external)
$sent = send_report($report)
```

| tag | meaning |
|---|---|
| `pure` | side-effect free |
| `read` | reads external state |
| `model` | invokes an LLM (non-deterministic) |
| `network` | general network egress |
| `write_file` | writes to the filesystem |
| `write_db` | writes to a database |
| `send_external` | sends email / message / webhook |
| `delete` | irreversibly deletes |
| `money` | moves money |
| `calendar` | mutates a calendar |
| `human_visible` | produces output a human will see |

Operations also declare their own effects host-side; the annotation is the plan author's
declaration of intent on a specific bind. See [Safety & approvals](../agent/safety.md) for how
effects feed the approval chain.

Tooling can ask **where** a flow's risk lives, per node: since 0.15.0,
`flux_lang::analyze::annotate_effects(&ast, &ops)` returns, for every `call` node (keyed by the
same node path diagnostics use, e.g. `body[3].then[1]`), its combined effects — the op's own
host-declared effects plus the `@effect(tag)` on its enclosing bind — with a risk tier and
idempotency. It is the per-node, attributed sibling of the flow-level effects union the approval
envelope consumes, so a visual editor or reviewer can pin exactly which call moves money instead
of only knowing that something in the flow does.

## Prelude artifact types

The prelude is an opt-in ontology of the artifacts agent work manipulates — claims, evidence,
needs, context packs, patches, structured returns. They are not new value kinds: every
artifact is an ordinary structured value whose named type points at a registered schema.

<!-- Generated from the same `flux_lang::prelude::prelude_type_catalog()` source of truth as
     crates/flux-lang/docs/reference.md and the SKILL.md language skills — do not hand-edit the
     table below. Regenerate with: `UPDATE=1 cargo test -p codewandler-flux-lang --test website_in_sync`. -->

The table below is derived from the generated prelude catalog in the repository's
[language reference](https://github.com/codewandler/flux/blob/main/crates/flux-lang/docs/reference.md).

<!-- BEGIN generated:prelude-types -->
| type | description |
|---|---|
| `Span` | A cited region inside a source document — the proof pointer a `Claim` or `Evidence` points at. |
| `Claim` | A factual assertion extracted from a source, carrying its provenance span and a confidence score. |
| `Evidence` | A claim together with the supporting spans that ground it — the audited unit of support. |
| `Need` | An explicit statement of missing information: what to ask, which fields are required to satisfy it, and the condition under which it is considered met. Produced by the pure `need` op; its complement `gaps` reports the still-unmet `require` fields. |
| `Ctx` | A bounded, intentionally-budgeted bundle of context — the value produced by the `ctx`/`ctx_append` nodes. `members` are the symbol references selected into the pack; at evaluation the runtime materializes their retained values into a model-ready payload, and `budget` caps that payload by character count. |
| `Query` | A structured retrieval request over one or more datasources — the input to the `query`/`Search.run` ops. |
| `Answer` | A structured, evidence-bearing **successful** return from an agent task. |
| `Blocked` | A structured return signalling the task **could not** be completed, with the open gaps that blocked it. Same shape as [`Answer`] but a distinct type so callers can branch on success vs. blockage. |
| `Patch` | A proposed code change — a concrete unified diff plus the path it applies to. |
| `TestResult` | The outcome of running a test command. |
| `Verdict` | A judge step's structured decision: the chosen outcome, the reasons behind it, and the evidence it weighed. Consumed by the `ai.judge` cognition op. |
<!-- END generated:prelude-types -->

The cognition operations produce and consume these types — `ai.extract` yields `Claim`s,
`ai.judge` yields a `Verdict`, `synth` assembles a cited `Answer`, and a task that cannot
finish returns `Blocked` instead. See [Operations](./ops.md).

## Truthiness

Condition positions use uniform JSON truthiness rather than type coercion rules — the table is
in the [execution model](./execution-model.md#truthiness).

## Related docs

- [Execution model](./execution-model.md) — values, truthiness, and dispatch.
- [Node reference](./node-reference.md) — where types and effects appear in JSON.
- [Operations](./ops.md) — ops that produce and consume prelude artifact types.
