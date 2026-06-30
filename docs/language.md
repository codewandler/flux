# Flux-Lang Semantics

Flux-Lang is the language pillar of flux: the typed plan format a model emits and a deterministic
runtime executes. It is deliberately small in purpose even though the node catalog has grown: it is an
agent working language, not a general-purpose programming language.

The shortest version:

> The model writes a readable, analyzer-validated plan over symbols. The runtime resolves those symbols
> to immutable values and dispatches every real-world effect through the same authorization -> approval
> -> guarded IO envelope.

## The Semantic Core

1. **A flow is an executable AST.** JSON `DraftAst`, native `.flux` text, and Rust DSL builders all
   describe the same tree of nodes. The lifecycle is compile or parse -> analyze/lower -> optionally
   optimize -> execute. The parser and formatter are tooling; the semantics live in the AST and the
   reference interpreter.
2. **Symbols are not values.** A symbol such as `$draft` names a value id in the session store. Values
   are immutable; a revision is another value id, not mutation in place. The model sees summaries,
   transcripts, and explicit context packs. The runtime alone dereferences values.
3. **`call` is the operation boundary.** The language knows how to call registered operations, but it
   does not own filesystem, process, provider, or network IO. The `flux-flow` engine adapts `call` onto
   `Executor::dispatch`, so policy, approval, redaction, and guarded IO remain the only production path
   for tool effects. Other host seams such as thing resolution, the value store, and the sink are
   injected traits, not concrete IO inside `flux-lang`. Pure nodes (`lit`, `var`, `fmt`, `jq`, `expr`,
   `obj`, `list`, `ctx`) do no IO.
4. **Control flow is structured and bounded.** Flows run top-to-bottom through explicit nodes:
   branch (`when`, `unless`, `match`, `route`), iterate (`repeat`, `each`, time-bounded `loop`), compose
   (`seq`, `pipe`, `fallback`), and run explicit concurrency (`parallel`, `race`). The analyzer rejects
   unbounded loops and top-level-only nodes in nested positions.
5. **Non-determinism is named.** Ordinary control flow is deterministic. Model choice is an explicit
   operation or a bounded `route`: the model may choose one declared label, never invent a new branch.
   External input is explicit `await`, which suspends at a top-level statement and resumes without
   re-running the completed prefix.
6. **Context is a first-class artifact.** `ctx` and `ctx_append` build budgeted context packs from
   existing symbols. Budgeting happens at node evaluation, before a consuming model op sees the pack.
   The artifact prelude (`Claim`, `Evidence`, `Need`, `Ctx`, `Answer`, `Patch`, etc.) gives agent work
   typed handles instead of burying all meaning in prose.
7. **Reliability is part of the language, not prompt advice.** `assert`, `try`, `retry`, `timeout`,
   `budget`, `confirm`, `scope`, `saga`, `once`, and `checkpoint` make common workflow constraints
   explicit: stop on invariant failure, recover from ordinary errors, bound cost/time, ask a human,
   guarantee cleanup, compensate partial side effects, and avoid duplicate side effects across reruns.

## What It Is Not

Flux-Lang is not a ReAct transcript where the LLM schedules each tool call live. It is not a shell
script: untrusted text never becomes a shell command, and process execution is an operation guarded by
the runtime. It is not a hand-wired dataflow DAG; authors write structured control flow and the
dependency DAG is derived from symbol reads. It is not a behavior tree: there is no root tick loop or
`Running` status protocol. It is not an actor/state-machine language; `await` is a suspend/resume point,
not a transition system.

## Programs And Composite Ops

A `.flux` module can also declare agents, channels, datasources, triggers, journeys, top-level flows,
and module-local composite ops. These declarations are pure data in `flux-lang`; L6 hosts give them
runtime meaning. A journey is still an ordinary flow. A composite op is a scoped sub-flow that can be
called like an operation when the host registers the module's composites; its inner calls still traverse
the same operation dispatch envelope.

## Where To Go Next

- [Architecture](architecture.md) explains where Flux-Lang sits in the workspace and safety envelope.
- [Agent loop](agent-loop.md) shows the self-hosted turn loop written as Flux-Lang.
- [Flux-Lang reference](../crates/flux-lang/docs/reference.md) lists every node and field.
- [Flux-Lang syntax](../crates/flux-lang/docs/syntax.md) specifies writable `.flux` text.
- [Flux-Lang status](../crates/flux-lang/docs/STATUS.md) tracks PRD conformance against the current tree.
