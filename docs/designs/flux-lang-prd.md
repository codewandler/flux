---
title: "Flux-Lang Module — Original PRD"
status: original-design
role: source-of-record
independent_of_flux_ecosystem: true
description: >-
  Original, standalone Product Requirements Document for Flux-Lang, authored independently of the
  existing flux ecosystem and preserved here verbatim as the source design. For how Flux-Lang is
  integrated into flux (the "LLM is not the runtime" execution engine, crate layout, safety reuse,
  and build plan), see flux-flow.md in this directory.
---

# Product Requirements Document: Flux-Lang Module

**Product:** Flux-Lang Module  
**Owner:** Timo Friedl  
**Status:** Draft PRD  
**Target implementation:** Rust module/crate  
**Last updated:** 2026-06-25

## 1. Executive Summary

Flux-Lang is a deterministic workflow language and runtime module for AI-assisted applications. It lets an LLM translate natural-language user requests into a constrained Abstract Syntax Tree (AST), while a Rust-based analyzer, optimizer, and interpreter perform resolution, validation, permission checks, execution, and trace logging.

The core design principle is:

```text
LLMs operate over symbols. The runtime operates over values.
```

Instead of repeatedly injecting full chat history, tool outputs, documents, contacts, or secrets into the model, Flux-Lang maintains an event-sourced session state with typed symbols, values, things, effects, and operation specifications. The LLM sees a compact symbolic view and emits a proposed AST. The runtime resolves placeholders to exact stored values and executes registered operations under policy control.

This produces a more secure, token-efficient, auditable, and optimizable alternative to conventional chat-agent orchestration.

## 2. Problem Statement

Current AI agent systems often use the LLM as the runtime scheduler. They serialize large tool outputs back into chat context, ask the model to decide the next action, and rely on the model to remember or reproduce previous results. This creates four major problems:

1. **Security risk:** external content can become prompt context and influence tool decisions.
2. **Token waste:** large values and tool outputs are repeatedly re-sent to the model.
3. **Non-determinism:** orchestration depends on free-form model reasoning turn by turn.
4. **Poor execution planning:** independent operations cannot easily be parallelized, cached, or optimized.

Flux-Lang addresses this by making the LLM a compiler front-end rather than the runtime. It converts natural language into a typed AST over placeholders. Rust owns identity, dereferencing, validation, effects, optimization, and execution.

## 3. Goals

### 3.1 Product Goals

- Provide a compact language and AST for deterministic AI workflow orchestration.
- Allow natural-language user turns to compile into typed workflow AST fragments.
- Maintain a session symbol table so users can refer to previous objects naturally, such as "the draft", "John", or "those results".
- Execute actions against stored values without requiring the LLM to regenerate or inspect full outputs.
- Support UI editing of flows as typed AST nodes rather than free-form text.
- Enable secure execution through type checking, effect checking, approval gates, and event logging.
- Enable performance optimization through dependency analysis, caching, batching, and parallel scheduling.

### 3.2 Engineering Goals

- Implement Flux-Lang as a modular Rust crate family.
- Make AST and HIR serializable and versionable.
- Make every operation registered with an explicit type signature, effect set, implementation handler, and policy metadata.
- Support both visual AST editing and compact syntax projection.
- Provide deterministic analyzer errors suitable for UI display.
- Provide replayable execution traces.

## 4. Non-Goals

Flux-Lang should not be:

- A general-purpose programming language.
- A replacement for Rust, JavaScript, Python, or existing workflow engines.
- A system where arbitrary model output can directly execute side effects.
- A system where model-visible chat history is the source of truth.
- A prompt-template-only framework.
- A free-form agent loop where the model is the scheduler.

The module should remain deliberately small: bind, call, branch, repeat, await, return, typed symbols, thing references, effects, and operation calls.

## 5. Core Concept

Flux-Lang separates natural-language interpretation from deterministic execution.

```text
Natural language
  -> LLM compiler
  -> Draft AST
  -> Analyzer
  -> Typed HIR
  -> Optimizer
  -> Physical execution plan
  -> Runtime
  -> Event log + updated session
```

The LLM may choose symbols and structure. The runtime resolves symbols and executes values.

Example user turn:

```text
Send the draft to John.
```

Model-visible symbolic context:

```text
$draft: Draft = "Renewal follow-up", status: unsent
$john: Person = "John Miller", relation: ACME contact
send.email(Person, Draft) -> SentMessage !SendExternal
```

Compiled syntax:

```text
send.email($john, $draft)
```

Runtime resolution:

```text
$john  -> thing:person_42
$draft -> value:draft_8
send.email(Person, Draft) -> SentMessage
Effect: SendExternal -> approval required unless policy allows
```

The LLM never needs the full email body or the full contact record unless a specific model operation requires it.

## 6. Target Users and Personas

### 6.1 Workflow Builder

A product or operations user who builds AI-powered automations visually. They need safe blocks, predictable execution, and clear error messages.

### 6.2 Developer

A Rust developer who registers operation handlers, defines types and schemas, implements resolvers, and integrates Flux-Lang into a product runtime.

### 6.3 Administrator / Security Owner

A stakeholder who defines policies, effect permissions, approval gates, data visibility rules, and audit requirements.

### 6.4 End User

A user interacting conversationally with a product. They expect natural follow-up behavior: "make it shorter", "send it", "use the previous file", "ask John".

## 7. Primary Use Cases

### 7.1 Use Case A: Slot Filling for Intent Detection

The system detects a user intent, extracts required slots, asks for missing values, merges replies into session state, and executes the intent when complete.

Example:

```text
User: I want to reschedule my delivery to next Friday.
```

Flow:

```text
Incoming Message
  -> Detect Intent
  -> Extract Slots
  -> Merge with Session Slots
  -> Required Slots Complete?
       yes -> Execute Intent
       no  -> Ask Missing Slot
              -> Await User Reply
              -> Loop
```

Flux syntax:

```text
flow SlotFill($msg: Message, $session: Session) -> IntentResult {
  $schema = @ctx("intent.schemas.support")
  $intent = intent.detect($msg, $schema) : IntentGuess !model
  $slots = slots.extract($msg, $intent.schema) : SlotMap !model
  $state = slots.merge($session.slots, $slots) : SlotState !pure

  repeat max 5 until $state.complete {
    $missing = slots.missing($state, $intent.schema) : MissingSlots !pure

    when $missing.empty {
      return ready($intent.name, $state)
    }

    $question = slots.ask($intent, $missing.next) : Message !model
    $reply = await user.message as Message
    $more = slots.extract($reply, $intent.schema) : SlotMap !model
    $state = slots.merge($state, $more) : SlotState !pure
  }

  return incomplete($intent.name, $state)
}
```

Session symbols after the first turn:

```text
$intent: IntentGuess = reschedule_delivery
$state: SlotState = {
  new_date: filled("next Friday"),
  delivery_id: missing
}
$question: Message = "Which delivery would you like to reschedule?"
```

When the user replies, the runtime resumes the awaited flow and updates only the slot state. It does not require the LLM to reconstruct previous tool outputs.

### 7.2 Use Case B: FAQ / Knowledge Base Query

The system answers a user question using retrieved evidence, not free-form memory.

Example:

```text
User: How do I reset my API key?
```

Flow:

```text
Incoming Question
  -> Normalize Query
  -> Search Knowledge Base
  -> Rerank Evidence
  -> Coverage Check
       weak -> Ask Clarifying Question / Escalate
       good -> Generate Answer from Evidence
               -> Grounding Check
               -> Return Answer with Sources
```

Flux syntax:

```text
flow KBAnswer($question: Message) -> AnswerResult {
  $kb = @ctx("kb.support.public")

  $query = kb.query($question) : SearchQuery !model
  $hits = kb.search($kb, $query, top: 8) : List<KbHit> !read
  $evidence = kb.rerank($question, $hits, top: 3) : EvidenceSet !model
  $coverage = kb.coverage($question, $evidence) : Coverage !pure

  when $coverage.score < .65 {
    $clarify = kb.clarify($question, $coverage.missing) : Message !model
    return needs_clarification($clarify)
  }

  $draft = kb.answer_from_evidence($question, $evidence) : DraftAnswer !model
  $check = kb.grounding_check($draft, $evidence) : GroundingCheck !pure

  when $check.ok {
    return answered($draft, citations: $evidence.sources)
  }

  return escalate($question, reason: $check.issues)
}
```

Key principle:

```text
The LLM may generate or rank evidence, but Rust retrieves, validates, grounds, and enforces policy.
```

## 8. Language Scope

Flux-Lang should support only a small set of constructs in v1.

| Construct | Purpose | Example |
|---|---|---|
| `flow` | Defines a named workflow | `flow Reply($ticket: Ticket) -> Result { ... }` |
| binding | Creates a typed symbol | `$draft = email.draft(...) : Draft` |
| call | Invokes a registered operation | `kb.search($kb, $query)` |
| thing ref | References external addressable objects | `@person("John")`, `@file("contract.pdf")` |
| branch | Conditional control flow | `when $check.ok { ... } else { ... }` |
| repeat | Bounded loop | `repeat max 5 until $state.complete { ... }` |
| await | Pauses until event/input | `$reply = await user.message as Message` |
| return | Ends the flow | `return answered($draft)` |
| effect | Declares operation effect | `!model`, `!read`, `!send` |

No `goto` in v1. Structured control flow only.

## 9. Core Data Model

### 9.1 Things

Things are addressable external objects. They may be unresolved, ambiguous, or resolved.

```rust
pub struct ThingRef {
    pub kind: ThingKind,
    pub selector: Selector,
}

pub enum ThingKind {
    Context,
    File,
    Person,
    Ticket,
    Email,
    Repo,
    Dataset,
    CalendarEvent,
    Url,
    Secret,
    Custom(String),
}

pub enum Selector {
    Id(String),
    Name(String),
    Path(String),
    Query(String),
    Key(String),
}
```

Resolution produces an exact identity:

```rust
pub struct ResolvedThing {
    pub id: ThingId,
    pub kind: ThingKind,
    pub display: String,
    pub source: Source,
    pub confidence: f32,
}
```

Rule:

```text
No side effects may execute until all required things are resolved unambiguously.
```

### 9.2 Values

Values are immutable runtime data produced by operations.

```rust
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Struct(BTreeMap<String, Value>),
    List(Vec<Value>),
    Thing(ResolvedThing),
    Ref(ValueRef),
}
```

A symbol points to a value or thing. A value is not mutated. Revisions produce new value IDs.

```text
$draft@1 -> value:draft_7
$draft@2 -> value:draft_8
$draft   -> value:draft_8
```

### 9.3 Symbols

The session symbol table is the primary context-management mechanism.

```rust
pub struct Binding {
    pub name: SymbolName,
    pub kind: BindingKind,
    pub ty: TypeRef,
    pub target: BindingTarget,
    pub visibility: Visibility,
    pub summary: String,
    pub provenance: Provenance,
    pub created_at: EventId,
    pub updated_at: Option<EventId>,
    pub status: BindingStatus,
}

pub enum BindingTarget {
    Value(ValueId),
    Thing(ThingId),
    Flow(FlowId),
    Op(OpName),
}
```

Visibility tiers:

```text
visible    can be referenced naturally
hidden     stored but not shown to LLM by default
pinned     always shown
expired    only accessible by explicit search
private    never shown to model unless explicitly required
```

### 9.4 Session State

```rust
pub struct Session {
    pub id: SessionId,
    pub symbols: SymbolTable,
    pub focus: FocusSet,
    pub values: ValueStore,
    pub things: ThingStore,
    pub events: EventLog,
}
```

A turn transforms session state:

```text
U + view(Session) -> DraftAST -> HIR -> Execute -> Session'
```

Where `view(Session)` is a compact, policy-filtered symbolic projection, not the full event log.

## 10. AST, HIR, and Execution Plan

### 10.1 Draft AST

The LLM emits Draft AST as JSON or compact syntax. Draft AST may contain unresolved symbols and thing references.

### 10.2 HIR

The analyzer lowers Draft AST into typed High-Level IR (HIR). HIR has resolved variables, type annotations, effect metadata, and structured control flow.

### 10.3 Physical Plan

The optimizer lowers HIR into a physical execution plan. It may reorder, batch, parallelize, cache, or skip operations when safe.

```rust
pub struct PhysicalPlan {
    pub stages: Vec<Stage>,
}

pub enum Stage {
    Parallel(Vec<NodeId>),
    Sequential(NodeId),
    Branch(BranchPlan),
    Repeat(RepeatPlan),
    Await(AwaitPlan),
    ApprovalFence(NodeId),
}
```

## 11. Operation Registry

Flux-Lang does not hardcode domain behavior such as `kb.rerank` or `slots.extract`. These are registered operations.

```rust
pub struct OpSpec {
    pub name: OpName,
    pub inputs: Vec<TypeRef>,
    pub output: TypeRef,
    pub effects: EffectSet,
    pub retry: RetryPolicy,
    pub idempotency: Idempotency,
    pub approval: ApprovalPolicy,
    pub cache_policy: CachePolicy,
    pub optimization: OptimizationHints,
}
```

Handler trait:

```rust
#[async_trait::async_trait]
pub trait OpHandler: Send + Sync {
    fn spec(&self) -> &OpSpec;

    async fn call(
        &self,
        args: Vec<Value>,
        ctx: &mut ExecCtx,
    ) -> Result<Value, FlowError>;
}
```

Operation names should express domain meaning. Effects express execution risk.

Prefer:

```text
kb.rerank($question, $hits) : EvidenceSet !model
slots.extract($msg, $schema) : SlotMap !model
email.draft($person, $summary) : Draft !model
policy.check_email($draft) : Check !pure
send.email($person, $draft) : SentMessage !send
```

Avoid making everything generic under `ai.*`.

## 12. Effects and Policy

Effects are first-class execution metadata.

```rust
pub enum Effect {
    Pure,
    Read,
    Model,
    Network,
    WriteFile,
    WriteDb,
    SendExternal,
    Delete,
    Money,
    Calendar,
    HumanVisible,
}
```

A policy can allow, deny, or require approval for effects.

Example policy:

```text
allow Pure
allow Read
allow Model
allow HumanVisible
require_approval SendExternal
deny Delete
deny Money
```

Policy checks happen before side effects. Approval fences may be inserted into the physical plan.

## 13. Context Management

Flux-Lang replaces long chat context with symbolic session context.

The LLM receives:

```text
User text
Visible symbols
Focus aliases
Available operations
Relevant type hints
```

The LLM does not receive:

```text
Full tool outputs
Full documents
Secrets
Full contact records
Full event log
Raw values unless required by a model operation
```

The execution runtime computes a dependency slice for each step.

Example:

```text
send.email($john, $draft)
```

LLM context:

```text
$john: Person = "John Miller"
$draft: Draft = "Renewal follow-up", unsent
```

Runtime context:

```text
thing:person_42 { email: john@example.com }
value:draft_8 { subject: ..., body: ... }
op_spec(send.email)
policy_snapshot
```

Design invariant:

```text
The LLM may reference values by symbol. Only the runtime may dereference values.
```

## 14. Security Requirements

### 14.1 Prompt Injection Resistance

External content may be used as data, but it must not become orchestration authority.

Requirements:

- Tool outputs and documents must be stored as values, not automatically appended to model context.
- Model operations must receive narrow, scoped inputs and output schemas.
- External content must not be allowed to rewrite policies, operation calls, or control flow.
- Any model-generated AST must pass analyzer validation.

### 14.2 Capability Security

- Every operation must declare effects.
- Policies must be enforced before execution.
- Side-effecting operations must support approval gates.
- Dangerous effects such as Delete and Money must be denied by default.
- The runtime must block unresolved or ambiguous thing references before side effects.

### 14.3 Auditability

Every execution must emit an immutable trace:

```rust
pub enum RunEvent {
    FlowStarted { run_id: RunId, flow_id: FlowId, inputs: ValueMap },
    ThingResolved { thing: ThingRef, resolved: ResolvedThing },
    StepStarted { step_id: StepId, op: OpName, input_hash: Hash },
    StepSucceeded { step_id: StepId, output: ValueRef },
    StepFailed { step_id: StepId, error: FlowError },
    ApprovalRequested { step_id: StepId, effects: Vec<Effect> },
    ApprovalGranted { step_id: StepId },
    FlowReturned { value: ValueRef },
}
```

## 15. Optimizer Requirements

Flux-Lang should support safe optimization once HIR is available.

Optimization is legal only if:

```text
data dependencies are preserved
effect constraints are preserved
approval/wait fences are preserved
policy behavior is unchanged
trace remains explainable
```

Supported optimization classes:

| Optimization | Description | Constraints |
|---|---|---|
| Parallel execution | Run independent read/model/pure ops concurrently | Effects must commute |
| Caching | Reuse value for same op version and input hashes | Op must be cacheable |
| Common subexpression elimination | Deduplicate identical pure/cacheable calls | No side effects |
| Dead step elimination | Remove unused pure/model outputs | Cannot remove required side effects |
| Batch fusion | Combine repeated compatible ops | Registered batch op required |
| Model-call fusion | Combine related model calls into one structured call | Explicit rewrite rule required |
| Predicate pushdown | Push filters into search/data operations | Semantics must match |
| Incremental recomputation | Recompute only changed dependency slice | Provenance graph required |

Example sequential flow:

```text
$hits = kb.search($kb, $query)
$profile = crm.lookup($customer)
$history = orders.history($customer)
$evidence = kb.rerank($question, $hits)
```

Optimized plan:

```text
parallel {
  $hits = kb.search($kb, $query)
  $profile = crm.lookup($customer)
  $history = orders.history($customer)
}
$evidence = kb.rerank($question, $hits)
```

## 16. UI Editor Requirements

The UI editor should be an AST editor, not primarily a text editor.

Principle:

```text
The UI edits typed nodes. Syntax is only a projection.
```

Required projections:

1. Visual flow canvas
2. Compact Flux syntax
3. JSON AST
4. Execution trace
5. Session symbols
6. Analyzer diagnostics

Core panes:

```text
Flow Canvas      Node Inspector
Session Symbols  Test / Trace
Op Palette       Type / Schema Browser
```

Node palette v1:

```text
Input
Bind
Call
When
Repeat
Await
Return
Thing
Policy / Approval Fence
```

The editor must prevent invalid AST states where possible and surface analyzer errors where not possible.

Example node inspector for `kb.rerank`:

```text
Operation: kb.rerank
Inputs:
  question: $question
  hits: $hits
Config:
  top: 3
  min_score: 0.65
Output:
  $evidence: EvidenceSet
Effects:
  Model
```

## 17. Public Rust API Requirements

### 17.1 Compile a User Turn

```rust
pub async fn compile_turn(
    user_text: &str,
    session_view: SessionView,
    registry: &OpRegistry,
    llm: &dyn LlmCompiler,
) -> Result<DraftAst, CompileError>;
```

### 17.2 Analyze AST

```rust
pub async fn analyze(
    ast: DraftAst,
    session: &Session,
    registry: &OpRegistry,
    policy: &dyn PolicyEngine,
) -> Result<HirFlow, AnalyzeError>;
```

### 17.3 Optimize HIR

```rust
pub fn optimize(
    hir: HirFlow,
    registry: &OpRegistry,
    options: OptimizerOptions,
) -> Result<PhysicalPlan, OptimizeError>;
```

### 17.4 Execute Plan

```rust
pub async fn execute(
    plan: PhysicalPlan,
    session: Session,
    runtime: &Runtime,
) -> Result<ExecutionResult, FlowError>;
```

### 17.5 Register Operation

```rust
pub fn register_op<H: OpHandler + 'static>(&mut self, handler: H);
```

## 18. Module Layout

Proposed Rust crate/module structure:

```text
flux-core
  ast.rs
  types.rs
  values.rs
  effects.rs
  ids.rs

flux-parse
  parser.rs
  pretty.rs
  json_schema.rs

flux-analyze
  names.rs
  things.rs
  types.rs
  effects.rs
  loops.rs
  policy.rs
  diagnostics.rs

flux-registry
  ops.rs
  schemas.rs
  things.rs
  rewrite_rules.rs

flux-runtime
  interpreter.rs
  planner.rs
  events.rs
  store.rs
  approvals.rs
  cache.rs

flux-llm
  compiler.rs
  prompts.rs
  repair.rs

flux-ui-model
  graph_projection.rs
  node_palette.rs
  inspector_schema.rs
```

## 19. MVP Scope

### 19.1 In Scope for MVP

- JSON AST schema.
- Compact syntax parser and pretty-printer for a minimal subset.
- Session symbol table with visibility and focus aliases.
- Thing references and deterministic resolver interface.
- Operation registry with type signatures and effect metadata.
- Analyzer passes: name resolution, type checking, effect checking, bounded loop checking.
- Interpreter for bind, call, when, repeat, await, return.
- Event log and value store.
- Basic policy engine with allow, deny, approval-required.
- LLM compiler adapter that emits Draft AST from user text and session view.
- UI-facing graph projection and node inspector metadata.
- Two example operation packs: slot filling and KB answer.

### 19.2 Out of Scope for MVP

- Full general-purpose parser features.
- Distributed execution.
- Complex transactions across external systems.
- Advanced optimizer rewrites beyond parallel read/model ops and caching.
- Long-running daemon/watch flows.
- Multi-tenant enterprise RBAC beyond basic policy hooks.
- Formal verification.

## 20. Acceptance Criteria

### 20.1 Language and AST

- Given valid compact Flux syntax, parser produces JSON AST.
- Given JSON AST, pretty-printer can render compact syntax.
- AST is serializable and versioned.
- Analyzer rejects unknown variables, unknown ops, type mismatch, forbidden effects, unbounded loops, and ambiguous things.

### 20.2 Session Context

- Runtime stores tool outputs as immutable values with IDs.
- LLM compiler receives symbolic session view, not full raw value store.
- User follow-ups such as "send it", "make it shorter", and "use the previous file" resolve through focus aliases and symbol table.
- Old value versions remain addressable for audit and undo.

### 20.3 Execution

- Runtime can execute bind, call, when, repeat, await, return.
- Runtime pauses on await and resumes with event input.
- Runtime blocks side effects until required approvals are granted.
- Runtime logs every step with input hashes and output refs.

### 20.4 Security

- Model-generated AST cannot execute unregistered operations.
- Model-generated AST cannot bypass effect policy.
- External content cannot directly introduce side-effecting control flow.
- Dangerous effects are denied by default.

### 20.5 Optimizer

- Independent pure/read/model ops can run in parallel.
- Cached values can be reused when op version and input hashes match.
- Side-effect fences are preserved.
- Optimizer emits an explainable optimization report.

### 20.6 UI Editor

- UI can render AST as visual graph.
- UI can edit node configuration without producing invalid AST shape.
- UI can show symbols, types, effects, and diagnostics.
- UI can show execution traces mapped back to graph nodes.

## 21. Example: End-to-End User Turn

User says:

```text
Send the draft to John.
```

Session view:

```text
focus:
  $draft = value:draft_8
  $person = thing:person_42

symbols:
  $draft: Draft = "Renewal follow-up", unsent
  $john: Person = "John Miller"

ops:
  send.email(Person, Draft) -> SentMessage !SendExternal
```

LLM emits Draft AST:

```json
{
  "kind": "call",
  "op": "send.email",
  "args": [
    { "kind": "var", "name": "john" },
    { "kind": "var", "name": "draft" }
  ]
}
```

Analyzer resolves:

```text
$john  -> thing:person_42 : Person
$draft -> value:draft_8 : Draft
send.email(Person, Draft) -> SentMessage !SendExternal
```

Policy result:

```text
SendExternal requires approval.
```

Runtime produces:

```text
ApprovalRequested(step: send.email, recipient: John Miller, draft: Renewal follow-up)
```

After approval:

```text
StepSucceeded(send.email) -> value:sent_message_12
$sent = value:sent_message_12
```

## 22. Risks and Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| LLM emits invalid AST | Poor UX | Schema-constrained output, repair loop, analyzer diagnostics |
| Too many domain ops | Registry sprawl | Domain packs plus generic internal primitives |
| Ambiguous references | Wrong execution target | Deterministic resolver, focus aliases, approval UI |
| Over-optimization changes semantics | Safety bug | Effect-aware optimizer and fences |
| Model ops leak sensitive raw data | Privacy issue | Visibility rules, scoped model inputs, policy checks |
| UI becomes too technical | Adoption risk | Visual node editor with syntax as optional projection |
| Rerank/extract ops feel too specialized | Maintenance burden | Domain-specific wrappers over generic reusable primitives |

## 23. Milestones

### Milestone 1: Core AST and Analyzer

- Define AST schema and Rust structs.
- Implement JSON parse/serialize.
- Implement compact syntax parser for core constructs.
- Implement name, type, loop, and effect checks.

### Milestone 2: Runtime and Store

- Implement value store and event log.
- Implement interpreter for v1 node types.
- Implement operation registry and handler trait.
- Implement policy engine with allow/deny/approval.

### Milestone 3: Session Context and LLM Compiler

- Implement session symbol table and focus aliases.
- Implement session view builder.
- Implement LLM compiler adapter for user turns.
- Implement validation and repair loop for Draft AST.

### Milestone 4: Example Operation Packs

- Implement slot-filling pack.
- Implement KB/FAQ pack.
- Implement sample flows and tests.

### Milestone 5: Optimizer MVP

- Implement dependency graph.
- Implement parallel scheduling for independent safe ops.
- Implement cache keys from op version and input hashes.
- Implement optimization report.

### Milestone 6: UI Integration Model

- Implement graph projection from AST/HIR.
- Implement node inspector schemas.
- Implement trace-to-node mapping.
- Provide sample editor fixtures.

## 24. Open Questions

1. Should compact syntax be public API or only a debug/review projection?
2. How strict should effect declarations in source syntax be if the op registry already knows effects?
3. Should thing resolution happen during analysis or lazily at execution time?
4. What is the minimum viable policy language for v1?
5. Should model operations support deterministic seeding and cacheable replay by default?
6. How should the UI represent revisions such as `$draft@1`, `$draft@2`, and current `$draft`?
7. Should saved flows be allowed to run without the LLM compiler after initial creation?
8. What should the standard library of domain operation packs include?

## 25. Summary

Flux-Lang is a deterministic workflow language and runtime for AI-assisted systems. It turns natural-language user turns into typed AST fragments, stores outputs as immutable values, exposes only symbolic placeholders to the LLM, and executes registered operations under a Rust analyzer, optimizer, policy engine, and interpreter.

Its main advantage over conventional chat-agent systems is the separation of responsibilities:

```text
LLM:      parse intent and propose structure
Analyzer: validate structure, names, types, effects, loops, policy
Runtime:  resolve values, execute operations, store outputs
Policy:   allow, deny, or require approval
Optimizer: reorder, cache, batch, and parallelize safely
Event log: remember exactly what happened
```

This makes AI workflows more secure, cheaper, faster, auditable, and easier to build visually.
