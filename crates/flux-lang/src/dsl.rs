//! A Rust **embedded DSL** for authoring Flux-Lang flows — builder primitives that compile straight
//! down to the [`crate::ast`] tree, so a Rust programmer can write a flow natively instead of emitting
//! JSON or natural language.
//!
//! The output is an ordinary [`DraftAst`]: feed it to the analyzer ([`crate::analyze`]) and the engine
//! (`flux-flow` / `flux-sdk`'s `FlowClient`) exactly as you would a model-compiled plan. The DSL is a
//! *construction* convenience only — it does **not** type-check the flow. Semantic validity (bounded
//! loops, top-level `await`, `match` subjects, `fallback` arity, op resolution) stays the analyzer's
//! job; build a flow here, then `analyze` it before you `execute`.
//!
//! Two layers:
//! - **Expression constructors** — free functions that return a [`Node`]: [`call`], [`var`], [`lit`],
//!   [`expr`], [`fmt`], [`jq`], [`parse_as`], [`thing`], [`peek`]. Use them anywhere a value/condition
//!   is wanted (a `call`'s argument, a `when`'s condition, a `bind`'s value).
//! - **Statement builders** — [`Flow`] (the top-level flow) and [`Block`] (a body accumulator) whose
//!   methods push statement nodes. Nodes that carry nested bodies take a closure
//!   (`impl FnOnce(&mut Block)` — or a small per-node builder for nodes with optional modifiers like
//!   `collect`/`until`/`bind`), so nesting reads as nesting.
//!
//! Two conventions worth knowing: a per-node builder's `body(..)`/`handler(..)` setter *replaces* (it
//! is meant to be called once), while [`Flow::body`] and the branch/include adders *accumulate*. And
//! `[]` for an empty argument list infers only on [`call`] (its element type is pinned to [`Node`]);
//! the generic symbol/var iterators ([`expr`], [`Block::ctx_append`], [`CtxBuilder::include`]) need a
//! typed empty like `Vec::<(&str, Node)>::new()` — but those empty cases are degenerate anyway.
//!
//! ```
//! use flux_lang::dsl::*;
//!
//! // each $f in $files -> $contents: read $f
//! let flow = Flow::named("scan")
//!     .param("files", TypeRef::List(Box::new(TypeRef::String)))
//!     .body(|b| {
//!         b.each("f", var("files"), |e| {
//!             e.collect("contents");
//!             e.body(|b| { b.call("read", [var("f")]); });
//!         });
//!         b.ret(var("contents"));
//!     })
//!     .build();
//!
//! assert_eq!(flow.name.as_deref(), Some("scan"));
//! assert_eq!(flow.body.len(), 2); // the `each` loop + the `return`
//! ```
#![warn(missing_docs)]

use serde_json::Value as Json;

use crate::ast::{
    Branch, FallbackBranch, FlowEffect, MatchCase, Param, RouteCase, SagaStep, SymbolName,
};
// Re-exported so an author can stay inside `flux_lang::dsl::*` for the whole flow — including the
// output type (`DraftAst`) and the raw `Node` (for the `Block::add` escape hatch).
pub use crate::ast::{DraftAst, Node, Selector, ThingKind, ThingRef, TypeRef};

/// Run a body-building closure against a fresh [`Block`] and return the accumulated nodes.
fn body_of(build: impl FnOnce(&mut Block)) -> Vec<Node> {
    let mut block = Block::new();
    build(&mut block);
    block.body
}

// ===========================================================================
// Expression constructors (free functions → Node)
// ===========================================================================

/// `op(args…)` — invoke a registered operation. `args` is any iterator of [`Node`]s (an array literal
/// works; `[]` for no arguments).
pub fn call(op: impl Into<String>, args: impl IntoIterator<Item = Node>) -> Node {
    Node::Call {
        op: op.into(),
        args: args.into_iter().collect(),
    }
}

/// `$name` — reference a bound session symbol.
pub fn var(name: impl Into<SymbolName>) -> Node {
    Node::Var { name: name.into() }
}

/// A literal value. Accepts anything `Into<serde_json::Value>` (`&str`, numbers, `bool`); for lists or
/// objects pass a [`serde_json::json!`] value.
pub fn lit(value: impl Into<Json>) -> Node {
    Node::Lit {
        value: value.into(),
    }
}

/// Pure inline arithmetic: a whitelisted `formula` over named `vars` (each a [`Node`], typically
/// [`var`] or [`lit`]). Example: `expr("price * 2", [("price", var("btc"))])`.
pub fn expr<K: Into<String>>(
    formula: impl Into<String>,
    vars: impl IntoIterator<Item = (K, Node)>,
) -> Node {
    Node::Expr {
        formula: formula.into(),
        vars: vars
            .into_iter()
            .map(|(k, v)| (k.into(), Box::new(v)))
            .collect(),
    }
}

/// Pure string interpolation: a `template` with `{symbol}` placeholders filled from bound symbols.
pub fn fmt(template: impl Into<String>) -> Node {
    Node::Fmt {
        template: template.into(),
    }
}

/// Pure JSON-path extraction: apply dot-path `path` to the JSON content of `input`. Example:
/// `jq(".bitcoin.usd", var("raw"))`.
pub fn jq(path: impl Into<String>, input: Node) -> Node {
    Node::Jq {
        path: path.into(),
        input: Box::new(input),
    }
}

/// Pure type coercion: convert the string result of `value` into a typed value. `as_type` is one of
/// `"f64"`, `"i64"`, `"bool"`, `"json"`, `"string"`. Named `parse_as` to avoid colliding with
/// [`crate::parse`].
pub fn parse_as(value: Node, as_type: impl Into<String>) -> Node {
    Node::Parse {
        value: Box::new(value),
        as_type: as_type.into(),
    }
}

/// A reference to an external thing (a file, person, ticket, url, …) addressed by a [`Selector`].
pub fn thing(kind: ThingKind, selector: Selector) -> Node {
    Node::Thing {
        thing: ThingRef { kind, selector },
    }
}

/// Read the current in-session value of a symbol without any IO (null if unbound).
pub fn peek(name: impl Into<SymbolName>) -> Node {
    Node::Peek { name: name.into() }
}

// ===========================================================================
// Flow — the top-level builder
// ===========================================================================

/// Build a flow body inline, returning the finished [`DraftAst`] (unnamed, no params). Shorthand for
/// `Flow::new().body(build).build()`.
pub fn flow(build: impl FnOnce(&mut Block)) -> DraftAst {
    Flow::new().body(build).build()
}

/// The top-level flow builder: an optional name, typed params, an optional return type, and a body.
/// Terminal [`build`](Self::build) yields the [`DraftAst`] the analyzer + engine consume.
#[derive(Debug, Default, Clone)]
pub struct Flow {
    ast: DraftAst,
}

impl Flow {
    /// A fresh, unnamed flow.
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh flow with `name`.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            ast: DraftAst {
                name: Some(name.into()),
                ..DraftAst::default()
            },
        }
    }

    /// Set (or replace) the flow name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.ast.name = Some(name.into());
        self
    }

    /// Declare a typed parameter (`$name: ty`).
    pub fn param(mut self, name: impl Into<SymbolName>, ty: TypeRef) -> Self {
        self.ast.params.push(Param {
            name: name.into(),
            ty,
        });
        self
    }

    /// Declare the flow's return type.
    pub fn returns(mut self, ty: TypeRef) -> Self {
        self.ast.returns = Some(ty);
        self
    }

    /// Append statements to the flow body (callable more than once; statements accumulate).
    pub fn body(mut self, build: impl FnOnce(&mut Block)) -> Self {
        self.ast.body.extend(body_of(build));
        self
    }

    /// Finish and return the [`DraftAst`].
    pub fn build(self) -> DraftAst {
        self.ast
    }
}

// ===========================================================================
// Block — the body accumulator
// ===========================================================================

/// A statement-list builder. Each method pushes one node and returns `&mut Self`, so statements chain.
/// Nodes with nested bodies take a closure; nodes with optional modifiers take a small per-node
/// builder closure (see [`EachBuilder`], [`RepeatBuilder`], …).
#[derive(Debug, Default, Clone)]
pub struct Block {
    body: Vec<Node>,
}

impl Block {
    /// An empty block.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push an already-constructed [`Node`] verbatim — the escape hatch for shapes the typed methods
    /// don't sugar.
    pub fn add(&mut self, node: Node) -> &mut Self {
        self.body.push(node);
        self
    }

    // ----- leaf statements -----

    /// `op(args…)` as a statement.
    pub fn call(
        &mut self,
        op: impl Into<String>,
        args: impl IntoIterator<Item = Node>,
    ) -> &mut Self {
        self.add(call(op, args))
    }

    /// `$name = value` — bind an expression to a symbol.
    pub fn bind(&mut self, name: impl Into<SymbolName>, value: Node) -> &mut Self {
        self.add(Node::Bind {
            name: name.into(),
            value: Box::new(value),
            ty: None,
            effect: None,
        })
    }

    /// `$name: ty = value` — bind with an explicit type hint.
    pub fn bind_typed(
        &mut self,
        name: impl Into<SymbolName>,
        ty: TypeRef,
        value: Node,
    ) -> &mut Self {
        self.add(Node::Bind {
            name: name.into(),
            value: Box::new(value),
            ty: Some(ty),
            effect: None,
        })
    }

    /// Bind with an explicit declared [`FlowEffect`] (advisory; drives risk/approval).
    pub fn bind_effect(
        &mut self,
        name: impl Into<SymbolName>,
        effect: FlowEffect,
        value: Node,
    ) -> &mut Self {
        self.add(Node::Bind {
            name: name.into(),
            value: Box::new(value),
            ty: None,
            effect: Some(effect),
        })
    }

    /// `memo $name = value` — compute-once-per-session: reuse the cached value if already resolved.
    ///
    /// To set *both* a type hint and an effect (a rare shape), build the [`Node::Memo`] directly and
    /// push it with [`add`](Self::add); the same applies to [`bind`](Self::bind).
    pub fn memo(&mut self, name: impl Into<SymbolName>, value: Node) -> &mut Self {
        self.add(Node::Memo {
            name: name.into(),
            value: Box::new(value),
            ty: None,
            effect: None,
        })
    }

    /// `memo $name: ty = value` — memo with an explicit type hint (mirrors [`bind_typed`](Self::bind_typed)).
    pub fn memo_typed(
        &mut self,
        name: impl Into<SymbolName>,
        ty: TypeRef,
        value: Node,
    ) -> &mut Self {
        self.add(Node::Memo {
            name: name.into(),
            value: Box::new(value),
            ty: Some(ty),
            effect: None,
        })
    }

    /// Memo with an explicit declared [`FlowEffect`] (mirrors [`bind_effect`](Self::bind_effect)).
    pub fn memo_effect(
        &mut self,
        name: impl Into<SymbolName>,
        effect: FlowEffect,
        value: Node,
    ) -> &mut Self {
        self.add(Node::Memo {
            name: name.into(),
            value: Box::new(value),
            ty: None,
            effect: Some(effect),
        })
    }

    /// `return value` — end the flow with a value.
    pub fn ret(&mut self, value: Node) -> &mut Self {
        self.add(Node::Return {
            value: Box::new(value),
        })
    }

    /// Assert `cond` is truthy, aborting the flow otherwise.
    pub fn assert(&mut self, cond: Node) -> &mut Self {
        self.add(Node::Assert {
            cond: Box::new(cond),
            message: None,
        })
    }

    /// Assert `cond` with a custom failure `message`.
    pub fn assert_msg(&mut self, cond: Node, message: impl Into<String>) -> &mut Self {
        self.add(Node::Assert {
            cond: Box::new(cond),
            message: Some(message.into()),
        })
    }

    /// Run `cmd` and assert its output contains `expect`, aborting otherwise.
    pub fn verify(&mut self, cmd: Node, expect: Node) -> &mut Self {
        self.add(Node::Verify {
            cmd: Box::new(cmd),
            expect: Box::new(expect),
            message: None,
        })
    }

    /// `verify` with a custom failure `message`.
    pub fn verify_msg(&mut self, cmd: Node, expect: Node, message: impl Into<String>) -> &mut Self {
        self.add(Node::Verify {
            cmd: Box::new(cmd),
            expect: Box::new(expect),
            message: Some(message.into()),
        })
    }

    /// Accrete more symbols (`add`) into an existing context pack `ctx` (the `+=` marker).
    pub fn ctx_append<S: Into<SymbolName>>(
        &mut self,
        ctx: impl Into<SymbolName>,
        add: impl IntoIterator<Item = S>,
    ) -> &mut Self {
        self.add(Node::CtxAppend {
            ctx: ctx.into(),
            add: add.into_iter().map(Into::into).collect(),
        })
    }

    // ----- simple control flow (direct body closures) -----

    /// `when cond: …` — run `then` when `cond` is truthy (no else branch).
    pub fn when(&mut self, cond: Node, then: impl FnOnce(&mut Block)) -> &mut Self {
        self.add(Node::When {
            cond: Box::new(cond),
            then: body_of(then),
            otherwise: Vec::new(),
        })
    }

    /// `when cond: … else: …` — run `then` when `cond` is truthy, else `otherwise`.
    pub fn when_else(
        &mut self,
        cond: Node,
        then: impl FnOnce(&mut Block),
        otherwise: impl FnOnce(&mut Block),
    ) -> &mut Self {
        self.add(Node::When {
            cond: Box::new(cond),
            then: body_of(then),
            otherwise: body_of(otherwise),
        })
    }

    /// `unless cond: …` — run `body` only when `cond` is falsey.
    pub fn unless(&mut self, cond: Node, body: impl FnOnce(&mut Block)) -> &mut Self {
        self.add(Node::Unless {
            cond: Box::new(cond),
            body: body_of(body),
        })
    }

    /// A sequential sub-block (no bound result).
    pub fn seq(&mut self, body: impl FnOnce(&mut Block)) -> &mut Self {
        self.add(Node::Seq {
            body: body_of(body),
            bind: None,
        })
    }

    /// A sequential sub-block binding its final result to `bind`.
    pub fn seq_bind(
        &mut self,
        bind: impl Into<SymbolName>,
        body: impl FnOnce(&mut Block),
    ) -> &mut Self {
        self.add(Node::Seq {
            body: body_of(body),
            bind: Some(bind.into()),
        })
    }

    /// A pipe chain: each step's output feeds the next step's first argument.
    pub fn pipe(&mut self, steps: impl IntoIterator<Item = Node>) -> &mut Self {
        self.add(Node::Pipe {
            steps: steps.into_iter().collect(),
            bind: None,
        })
    }

    /// A pipe chain binding the final result to `bind`.
    pub fn pipe_bind(
        &mut self,
        bind: impl Into<SymbolName>,
        steps: impl IntoIterator<Item = Node>,
    ) -> &mut Self {
        self.add(Node::Pipe {
            steps: steps.into_iter().collect(),
            bind: Some(bind.into()),
        })
    }

    /// A human-in-the-loop gate: run `body` only on approval.
    pub fn confirm(
        &mut self,
        message: impl Into<String>,
        body: impl FnOnce(&mut Block),
    ) -> &mut Self {
        self.add(Node::Confirm {
            message: message.into(),
            risk: None,
            body: body_of(body),
        })
    }

    /// A confirm gate tagged with a `risk` level (`"low"`|`"medium"`|`"high"`|`"critical"`).
    pub fn confirm_risk(
        &mut self,
        message: impl Into<String>,
        risk: impl Into<String>,
        body: impl FnOnce(&mut Block),
    ) -> &mut Self {
        self.add(Node::Confirm {
            message: message.into(),
            risk: Some(risk.into()),
            body: body_of(body),
        })
    }

    /// Rate-limit `body`: at most `max` dispatches per `window_ms`, keyed by the stable `name`.
    pub fn throttle(
        &mut self,
        name: impl Into<String>,
        max: u32,
        window_ms: u64,
        body: impl FnOnce(&mut Block),
    ) -> &mut Self {
        self.add(Node::Throttle {
            name: name.into(),
            max,
            window_ms,
            body: body_of(body),
        })
    }

    /// Debounce `body`: wait `wait_ms` after the last trigger before running, keyed by `name`.
    pub fn debounce(
        &mut self,
        name: impl Into<String>,
        wait_ms: u64,
        body: impl FnOnce(&mut Block),
    ) -> &mut Self {
        self.add(Node::Debounce {
            name: name.into(),
            wait_ms,
            body: body_of(body),
        })
    }

    // ----- loops -----

    /// `each $item in source: …` — list-driven loop. Configure the body (and optional `collect`/`flat`)
    /// on the [`EachBuilder`].
    pub fn each(
        &mut self,
        item: impl Into<SymbolName>,
        source: Node,
        build: impl FnOnce(&mut EachBuilder),
    ) -> &mut Self {
        let mut e = EachBuilder::default();
        build(&mut e);
        self.add(Node::Each {
            source: Box::new(source),
            item: item.into(),
            body: e.body,
            collect: e.collect,
            flat: e.flat,
        })
    }

    /// `repeat max: …` — bounded counter loop. Configure body / `until` / `collect` on the
    /// [`RepeatBuilder`].
    pub fn repeat(&mut self, max: u32, build: impl FnOnce(&mut RepeatBuilder)) -> &mut Self {
        let mut r = RepeatBuilder::default();
        build(&mut r);
        self.add(Node::Repeat {
            max,
            until: r.until.map(Box::new),
            body: r.body,
            collect: r.collect,
        })
    }

    /// `loop for_ms: …` — time-bounded loop. Configure body / `every_ms` / `until` / `bind` on the
    /// [`LoopBuilder`].
    pub fn loop_for(&mut self, for_ms: u64, build: impl FnOnce(&mut LoopBuilder)) -> &mut Self {
        let mut l = LoopBuilder::default();
        build(&mut l);
        self.add(Node::Loop {
            for_ms,
            every_ms: l.every_ms,
            until: l.until.map(Box::new),
            body: l.body,
            bind: l.bind,
        })
    }

    /// `retry max: …` — retry the body on failure. Configure body / `backoff` / `delay_ms` / `bind` on
    /// the [`RetryBuilder`].
    pub fn retry(&mut self, max: u32, build: impl FnOnce(&mut RetryBuilder)) -> &mut Self {
        let mut r = RetryBuilder::default();
        build(&mut r);
        self.add(Node::Retry {
            max,
            backoff: r.backoff,
            delay_ms: r.delay_ms,
            body: r.body,
            bind: r.bind,
        })
    }

    /// Concurrent fan-out: declare named branches on the [`Branches`] builder; each branch's result
    /// binds to its name.
    pub fn parallel(&mut self, build: impl FnOnce(&mut Branches)) -> &mut Self {
        let mut br = Branches::default();
        build(&mut br);
        self.add(Node::Parallel {
            branches: br.branches,
        })
    }

    /// First-wins concurrency: run branches in parallel, returning the first success within
    /// `timeout_ms`. Configure branches / `bind` on the [`RaceBuilder`].
    pub fn race(&mut self, timeout_ms: u64, build: impl FnOnce(&mut RaceBuilder)) -> &mut Self {
        let mut r = RaceBuilder::default();
        build(&mut r);
        self.add(Node::Race {
            timeout_ms,
            branches: r.branches,
            bind: r.bind,
        })
    }

    // ----- suspension / context / structured errors -----

    /// `await source` — suspend until an external input arrives. Configure `bind` / `as_type` on the
    /// [`AwaitBuilder`]. Must be a top-level statement (the analyzer enforces this).
    pub fn await_(
        &mut self,
        source: impl Into<String>,
        build: impl FnOnce(&mut AwaitBuilder),
    ) -> &mut Self {
        let mut a = AwaitBuilder::default();
        build(&mut a);
        self.add(Node::Await {
            binding: a.binding,
            source: source.into(),
            as_type: a.as_type,
        })
    }

    /// Build a bounded **context pack** bound to `name`. Configure purpose / include / exclude / budget
    /// on the [`CtxBuilder`].
    pub fn ctx(
        &mut self,
        name: impl Into<SymbolName>,
        build: impl FnOnce(&mut CtxBuilder),
    ) -> &mut Self {
        let mut c = CtxBuilder::default();
        build(&mut c);
        self.add(Node::Ctx {
            name: name.into(),
            purpose: c.purpose,
            include: c.include,
            exclude: c.exclude,
            budget: c.budget,
        })
    }

    /// Structured error handling: configure `body` / `catch` / `handler` on the [`TryBuilder`].
    pub fn try_(&mut self, build: impl FnOnce(&mut TryBuilder)) -> &mut Self {
        let mut t = TryBuilder::default();
        build(&mut t);
        self.add(Node::Try {
            body: t.body,
            catch: t.catch,
            handler: t.handler,
        })
    }

    // ----- P6 control-flow primitives -----

    /// `match subject { … }` — exhaustive multi-way branch by JSON equality. Declare cases / default on
    /// the [`MatchArms`] builder.
    ///
    /// The analyzer requires `subject` to be a [`var`] or [`lit`] (bind an op's result first, or use
    /// [`route`](Self::route) to branch on a model decision) and at least one case; an arbitrary
    /// `subject` node builds but will be rejected at `analyze`.
    pub fn match_(&mut self, subject: Node, build: impl FnOnce(&mut MatchArms)) -> &mut Self {
        // Struct literal, not `MatchArms::default()`: the builder's `.default()` arm-setter shadows the
        // `Default::default` associated fn at the path-call site.
        let mut m = MatchArms {
            cases: Vec::new(),
            default: Vec::new(),
        };
        build(&mut m);
        self.add(Node::Match {
            subject: Box::new(subject),
            cases: m.cases,
            default: m.default,
        })
    }

    /// `route selector { … }` — model-routed branch: the selector yields a label naming a fixed case.
    /// Declare cases / default on the [`RouteArms`] builder.
    pub fn route(&mut self, selector: Node, build: impl FnOnce(&mut RouteArms)) -> &mut Self {
        // Struct literal, not `RouteArms::default()` — see `match_` for why.
        let mut r = RouteArms {
            cases: Vec::new(),
            default: Vec::new(),
        };
        build(&mut r);
        self.add(Node::Route {
            selector: Box::new(selector),
            cases: r.cases,
            default: r.default,
        })
    }

    /// `fallback { … }` — ordered first-success selector. Declare branches / `bind` on the
    /// [`FallbackBuilder`].
    pub fn fallback(&mut self, build: impl FnOnce(&mut FallbackBuilder)) -> &mut Self {
        let mut f = FallbackBuilder::default();
        build(&mut f);
        self.add(Node::Fallback {
            branches: f.branches,
            bind: f.bind,
        })
    }

    /// `timeout ms: …` — bound the wall-clock of a sub-flow. Configure body / `bind` on the
    /// [`WrapBuilder`].
    pub fn timeout(&mut self, ms: u64, build: impl FnOnce(&mut WrapBuilder)) -> &mut Self {
        let mut w = WrapBuilder::default();
        build(&mut w);
        self.add(Node::Timeout {
            ms,
            body: w.body,
            bind: w.bind,
        })
    }

    /// `budget limit: …` — cap the op-dispatch count of a scope. Configure body / `bind` on the
    /// [`WrapBuilder`].
    pub fn budget(&mut self, limit: u32, build: impl FnOnce(&mut WrapBuilder)) -> &mut Self {
        let mut w = WrapBuilder::default();
        build(&mut w);
        self.add(Node::Budget {
            limit,
            body: w.body,
            bind: w.bind,
        })
    }

    /// `scope: acquire … / body / finally …` — RAII acquire→use→release with guaranteed cleanup.
    /// Configure the optional `acquire` (+ bind), the `body`, and the `finally` on the [`ScopeBuilder`].
    pub fn scope(&mut self, build: impl FnOnce(&mut ScopeBuilder)) -> &mut Self {
        let mut s = ScopeBuilder::default();
        build(&mut s);
        self.add(Node::Scope {
            acquire: s.acquire.map(Box::new),
            bind: s.bind,
            body: s.body,
            finally: s.finally,
        })
    }

    /// `saga: step … undo … / step …` — a compensating transaction that unwinds completed steps in
    /// reverse if a later one fails. Configure the steps on the [`SagaBuilder`].
    pub fn saga(&mut self, build: impl FnOnce(&mut SagaBuilder)) -> &mut Self {
        let mut s = SagaBuilder::default();
        build(&mut s);
        self.add(Node::Saga { steps: s.steps })
    }

    /// `once "label": …` — run the body at most once across re-runs (an effect-level memo, keyed on
    /// the idempotency `label`). Configure the body on the [`WrapBuilder`].
    pub fn once(
        &mut self,
        label: impl Into<String>,
        build: impl FnOnce(&mut WrapBuilder),
    ) -> &mut Self {
        let mut w = WrapBuilder::default();
        build(&mut w);
        self.add(Node::Once {
            label: label.into(),
            body: w.body,
            bind: w.bind,
        })
    }

    /// `checkpoint "label"` — a durable, top-level resume point; a re-run of the same flow in the
    /// same session fast-forwards past the completed prefix.
    pub fn checkpoint(&mut self, label: impl Into<String>) -> &mut Self {
        self.add(Node::Checkpoint {
            label: label.into(),
        })
    }

    /// Consume the block, yielding its accumulated statements.
    pub fn into_body(self) -> Vec<Node> {
        self.body
    }
}

// ===========================================================================
// Per-node builders (for nodes with optional modifiers)
// ===========================================================================

/// Configures a [`Block::scope`]: an optional resource `acquire` (+ bind), the `body`, and the
/// `finally` cleanup that always runs.
#[derive(Debug, Default, Clone)]
pub struct ScopeBuilder {
    acquire: Option<Node>,
    bind: Option<SymbolName>,
    body: Vec<Node>,
    finally: Vec<Node>,
}

impl ScopeBuilder {
    /// Acquire a resource by running `node`, binding its result to `name` for `body`/`finally`.
    pub fn acquire(&mut self, name: impl Into<SymbolName>, node: Node) -> &mut Self {
        self.acquire = Some(node);
        self.bind = Some(name.into());
        self
    }
    /// The body that uses the acquired resource.
    pub fn body(&mut self, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.body = body_of(build);
        self
    }
    /// The cleanup that always runs — on success, an early `return`, or an error.
    pub fn finally(&mut self, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.finally = body_of(build);
        self
    }
}

/// Configures a [`Block::saga`]: an ordered list of compensable steps.
#[derive(Debug, Default, Clone)]
pub struct SagaBuilder {
    steps: Vec<SagaStep>,
}

impl SagaBuilder {
    /// Add a step — its forward `body` and an optional compensating `undo` (via [`SagaStepBuilder`]).
    pub fn step(&mut self, build: impl FnOnce(&mut SagaStepBuilder)) -> &mut Self {
        let mut s = SagaStepBuilder::default();
        build(&mut s);
        self.steps.push(SagaStep {
            body: s.body,
            undo: s.undo,
        });
        self
    }
}

/// Configures one [`SagaBuilder::step`]: the forward `body` and the compensating `undo`.
#[derive(Debug, Default, Clone)]
pub struct SagaStepBuilder {
    body: Vec<Node>,
    undo: Vec<Node>,
}

impl SagaStepBuilder {
    /// The forward work this step performs.
    pub fn body(&mut self, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.body = body_of(build);
        self
    }
    /// The compensation, run (in reverse order) if a *later* step fails.
    pub fn undo(&mut self, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.undo = body_of(build);
        self
    }
}

/// Configures a [`Block::each`] loop: its body plus the optional `collect`/`flat` modifiers.
#[derive(Debug, Default, Clone)]
pub struct EachBuilder {
    body: Vec<Node>,
    collect: Option<SymbolName>,
    flat: bool,
}

impl EachBuilder {
    /// The loop body, run once per element (the element is bound to the `each`'s item symbol).
    pub fn body(&mut self, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.body = body_of(build);
        self
    }
    /// Gather each iteration's result into a list bound to `sym`.
    pub fn collect(&mut self, sym: impl Into<SymbolName>) -> &mut Self {
        self.collect = Some(sym.into());
        self
    }
    /// Flatten the collected results one level (each iteration must yield a list).
    pub fn flat(&mut self) -> &mut Self {
        self.flat = true;
        self
    }
}

/// Configures a [`Block::repeat`] loop: its body plus the optional `until`/`collect` modifiers.
#[derive(Debug, Default, Clone)]
pub struct RepeatBuilder {
    body: Vec<Node>,
    until: Option<Node>,
    collect: Option<SymbolName>,
}

impl RepeatBuilder {
    /// The loop body.
    pub fn body(&mut self, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.body = body_of(build);
        self
    }
    /// Early-exit when `cond` becomes truthy.
    pub fn until(&mut self, cond: Node) -> &mut Self {
        self.until = Some(cond);
        self
    }
    /// Gather each iteration's last result into a list bound to `sym`.
    pub fn collect(&mut self, sym: impl Into<SymbolName>) -> &mut Self {
        self.collect = Some(sym.into());
        self
    }
}

/// Configures a [`Block::loop_for`] time-bounded loop.
#[derive(Debug, Default, Clone)]
pub struct LoopBuilder {
    body: Vec<Node>,
    every_ms: u64,
    until: Option<Node>,
    bind: Option<SymbolName>,
}

impl LoopBuilder {
    /// The loop body.
    pub fn body(&mut self, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.body = body_of(build);
        self
    }
    /// Inter-iteration sleep in milliseconds (0 = tight loop).
    pub fn every_ms(&mut self, ms: u64) -> &mut Self {
        self.every_ms = ms;
        self
    }
    /// Early-exit when `cond` becomes truthy.
    pub fn until(&mut self, cond: Node) -> &mut Self {
        self.until = Some(cond);
        self
    }
    /// Bind the loop's final result to `sym`.
    pub fn bind(&mut self, sym: impl Into<SymbolName>) -> &mut Self {
        self.bind = Some(sym.into());
        self
    }
}

/// Configures a [`Block::retry`] block.
#[derive(Debug, Default, Clone)]
pub struct RetryBuilder {
    body: Vec<Node>,
    backoff: Option<String>,
    delay_ms: Option<u64>,
    bind: Option<SymbolName>,
}

impl RetryBuilder {
    /// The body to retry on failure.
    pub fn body(&mut self, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.body = body_of(build);
        self
    }
    /// Backoff strategy: `"none"` | `"linear"` | `"exponential"`.
    pub fn backoff(&mut self, strategy: impl Into<String>) -> &mut Self {
        self.backoff = Some(strategy.into());
        self
    }
    /// Base inter-attempt delay in milliseconds.
    pub fn delay_ms(&mut self, ms: u64) -> &mut Self {
        self.delay_ms = Some(ms);
        self
    }
    /// Bind the successful attempt's result to `sym`.
    pub fn bind(&mut self, sym: impl Into<SymbolName>) -> &mut Self {
        self.bind = Some(sym.into());
        self
    }
}

/// Configures a [`Block::try_`] block: body, the optional `catch` symbol, and the handler.
#[derive(Debug, Default, Clone)]
pub struct TryBuilder {
    body: Vec<Node>,
    catch: Option<SymbolName>,
    handler: Vec<Node>,
}

impl TryBuilder {
    /// The protected body.
    pub fn body(&mut self, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.body = body_of(build);
        self
    }
    /// Bind the error string to `sym` for the handler.
    pub fn catch(&mut self, sym: impl Into<SymbolName>) -> &mut Self {
        self.catch = Some(sym.into());
        self
    }
    /// The recovery handler, run when the body errors.
    pub fn handler(&mut self, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.handler = body_of(build);
        self
    }
}

/// Configures a [`Block::await_`]: the optional bound symbol and coercion type.
#[derive(Debug, Default, Clone)]
pub struct AwaitBuilder {
    binding: Option<SymbolName>,
    as_type: Option<TypeRef>,
}

impl AwaitBuilder {
    /// Bind the awaited input to `sym`.
    pub fn bind(&mut self, sym: impl Into<SymbolName>) -> &mut Self {
        self.binding = Some(sym.into());
        self
    }
    /// Coerce the awaited input to `ty`.
    pub fn as_type(&mut self, ty: TypeRef) -> &mut Self {
        self.as_type = Some(ty);
        self
    }
}

/// Configures a [`Block::ctx`] context pack.
#[derive(Debug, Default, Clone)]
pub struct CtxBuilder {
    purpose: Option<String>,
    include: Vec<SymbolName>,
    exclude: Vec<SymbolName>,
    budget: Option<u64>,
}

impl CtxBuilder {
    /// A human-readable purpose for the pack.
    pub fn purpose(&mut self, purpose: impl Into<String>) -> &mut Self {
        self.purpose = Some(purpose.into());
        self
    }
    /// Symbols to include in the pack.
    pub fn include<S: Into<SymbolName>>(&mut self, syms: impl IntoIterator<Item = S>) -> &mut Self {
        self.include.extend(syms.into_iter().map(Into::into));
        self
    }
    /// Symbols to exclude from the pack.
    pub fn exclude<S: Into<SymbolName>>(&mut self, syms: impl IntoIterator<Item = S>) -> &mut Self {
        self.exclude.extend(syms.into_iter().map(Into::into));
        self
    }
    /// Char budget; the pack shrinks at evaluation to fit.
    pub fn budget(&mut self, chars: u64) -> &mut Self {
        self.budget = Some(chars);
        self
    }
}

/// Declares the named branches of a [`Block::parallel`] fan-out.
#[derive(Debug, Default, Clone)]
pub struct Branches {
    branches: Vec<Branch>,
}

impl Branches {
    /// Add a branch named `name` whose body is built by `build`.
    pub fn branch(
        &mut self,
        name: impl Into<SymbolName>,
        build: impl FnOnce(&mut Block),
    ) -> &mut Self {
        self.branches.push(Branch {
            name: name.into(),
            body: body_of(build),
        });
        self
    }
}

/// Declares the branches (and optional `bind`) of a [`Block::race`].
#[derive(Debug, Default, Clone)]
pub struct RaceBuilder {
    branches: Vec<Branch>,
    bind: Option<SymbolName>,
}

impl RaceBuilder {
    /// Add a racing branch named `name`.
    pub fn branch(
        &mut self,
        name: impl Into<SymbolName>,
        build: impl FnOnce(&mut Block),
    ) -> &mut Self {
        self.branches.push(Branch {
            name: name.into(),
            body: body_of(build),
        });
        self
    }
    /// Bind the winning branch's result to `sym`.
    pub fn bind(&mut self, sym: impl Into<SymbolName>) -> &mut Self {
        self.bind = Some(sym.into());
        self
    }
}

/// Declares the cases (and optional default) of a [`Block::match_`]. Constructed via a struct literal
/// in [`Block::match_`] (not `Default`) because the `.default()` arm-setter shadows `Default::default`.
#[derive(Debug, Clone)]
pub struct MatchArms {
    cases: Vec<MatchCase>,
    default: Vec<Node>,
}

impl MatchArms {
    /// A case: run `build` when the subject JSON-equals `value` (a [`lit`] or [`var`]).
    pub fn case(&mut self, value: Node, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.cases.push(MatchCase {
            value,
            body: body_of(build),
        });
        self
    }
    /// The default arm, run when no case matches.
    pub fn default(&mut self, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.default = body_of(build);
        self
    }
}

/// Declares the cases (and optional default) of a [`Block::route`]. Constructed via a struct literal in
/// [`Block::route`] (not `Default`) — see [`MatchArms`].
#[derive(Debug, Clone)]
pub struct RouteArms {
    cases: Vec<RouteCase>,
    default: Vec<Node>,
}

impl RouteArms {
    /// A case: run `build` when the selector yields the string `label`.
    pub fn case(&mut self, label: impl Into<String>, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.cases.push(RouteCase {
            label: label.into(),
            body: body_of(build),
        });
        self
    }
    /// The default arm, run when the label matches no case.
    pub fn default(&mut self, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.default = body_of(build);
        self
    }
}

/// Declares the ordered branches (and optional `bind`) of a [`Block::fallback`].
#[derive(Debug, Default, Clone)]
pub struct FallbackBuilder {
    branches: Vec<FallbackBranch>,
    bind: Option<SymbolName>,
}

impl FallbackBuilder {
    /// Add a branch tried in declared order until one succeeds.
    pub fn branch(&mut self, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.branches.push(FallbackBranch {
            body: body_of(build),
        });
        self
    }
    /// Bind the winning branch's result to `sym`.
    pub fn bind(&mut self, sym: impl Into<SymbolName>) -> &mut Self {
        self.bind = Some(sym.into());
        self
    }
}

/// Configures a body-wrapping guard ([`Block::timeout`] / [`Block::budget`]): the body and optional
/// result `bind`.
#[derive(Debug, Default, Clone)]
pub struct WrapBuilder {
    body: Vec<Node>,
    bind: Option<SymbolName>,
}

impl WrapBuilder {
    /// The wrapped body.
    pub fn body(&mut self, build: impl FnOnce(&mut Block)) -> &mut Self {
        self.body = body_of(build);
        self
    }
    /// Bind the body's result to `sym`.
    pub fn bind(&mut self, sym: impl Into<SymbolName>) -> &mut Self {
        self.bind = Some(sym.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::ThingKind;
    use serde_json::json;

    #[test]
    fn expression_constructors_build_the_right_nodes() {
        assert_eq!(
            call("read", [lit("x")]),
            Node::Call {
                op: "read".into(),
                args: vec![Node::Lit { value: json!("x") }],
            }
        );
        assert_eq!(
            var("draft"),
            Node::Var {
                name: "draft".into()
            }
        );
        assert_eq!(lit(3), Node::Lit { value: json!(3) });
        assert_eq!(
            jq(".a", var("raw")),
            Node::Jq {
                path: ".a".into(),
                input: Box::new(Node::Var { name: "raw".into() }),
            }
        );
        assert_eq!(
            parse_as(fmt("{p}"), "f64"),
            Node::Parse {
                value: Box::new(Node::Fmt {
                    template: "{p}".into()
                }),
                as_type: "f64".into(),
            }
        );
        // expr carries its var map.
        let e = expr("price * 2", [("price", var("btc"))]);
        let Node::Expr { formula, vars } = e else {
            panic!("expected Expr")
        };
        assert_eq!(formula, "price * 2");
        assert_eq!(vars["price"], Box::new(Node::Var { name: "btc".into() }));
    }

    #[test]
    fn block_builds_a_loop_with_collect() {
        let ast = flow(|b| {
            b.each("f", var("files"), |e| {
                e.collect("contents");
                e.body(|b| {
                    b.call("read", [var("f")]);
                });
            });
            b.ret(var("contents"));
        });
        assert_eq!(
            ast.body[0],
            Node::Each {
                source: Box::new(Node::Var {
                    name: "files".into()
                }),
                item: "f".into(),
                body: vec![Node::Call {
                    op: "read".into(),
                    args: vec![Node::Var { name: "f".into() }],
                }],
                collect: Some("contents".into()),
                flat: false,
            }
        );
        assert!(matches!(ast.body[1], Node::Return { .. }));
    }

    #[test]
    fn match_and_route_arms_compile() {
        let ast = flow(|b| {
            b.bind("k", lit("a"));
            b.match_(var("k"), |m| {
                m.case(lit("a"), |b| {
                    b.call("noop", []);
                });
                m.default(|b| {
                    b.ret(lit("none"));
                });
            });
        });
        let Node::Match {
            subject,
            cases,
            default,
        } = &ast.body[1]
        else {
            panic!("expected Match")
        };
        assert_eq!(**subject, Node::Var { name: "k".into() });
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].value, Node::Lit { value: json!("a") });
        assert_eq!(default.len(), 1);
    }

    #[test]
    fn flow_named_with_params_and_returns() {
        let ast = Flow::named("reply")
            .param("ticket", TypeRef::Named("Ticket".into()))
            .returns(TypeRef::String)
            .body(|b| {
                b.ret(var("ticket"));
            })
            .build();
        assert_eq!(ast.name.as_deref(), Some("reply"));
        assert_eq!(ast.params.len(), 1);
        assert_eq!(ast.params[0].ty, TypeRef::Named("Ticket".into()));
        assert_eq!(ast.returns, Some(TypeRef::String));
    }

    /// The headline: a loop-heavy flow using every loop + P6 control-flow primitive round-trips through
    /// the canonical text syntax unchanged (`parse(format(ast)) == ast`).
    #[test]
    fn all_loops_showcase_round_trips_through_text() {
        let ast = Flow::named("showcase").body(|b| {
            b.bind("files", lit(json!(["a.rs", "b.rs"])));
            b.each("f", var("files"), |e| {
                e.collect("reads").flat();
                e.body(|b| {
                    b.call("read", [var("f")]);
                });
            });
            b.repeat(3, |r| {
                r.until(var("done")).collect("rounds");
                r.body(|b| {
                    b.call("step", []);
                });
            });
            b.loop_for(1000, |l| {
                l.every_ms(100).bind("ticks");
                l.body(|b| {
                    b.call("tick", []);
                });
            });
            b.race(500, |r| {
                r.bind("winner");
                r.branch("fast", |b| {
                    b.call("a", []);
                });
                r.branch("slow", |b| {
                    b.call("bee", []);
                });
            });
            b.match_(var("done"), |m| {
                m.case(lit(true), |b| {
                    b.ret(lit("yes"));
                });
                m.default(|b| {
                    b.call("noop", []);
                });
            });
            b.route(call("classify", [var("files")]), |r| {
                r.case("hot", |b| {
                    b.call("fast_path", []);
                });
                r.default(|b| {
                    b.call("slow_path", []);
                });
            });
            b.fallback(|f| {
                f.bind("got");
                f.branch(|b| {
                    b.call("cheap", []);
                });
                f.branch(|b| {
                    b.call("expensive", []);
                });
            });
            b.timeout(2000, |w| {
                w.bind("t");
                w.body(|b| {
                    b.call("slow", []);
                });
            });
            b.budget(5, |w| {
                w.body(|b| {
                    b.call("bounded", []);
                });
            });
        });
        let ast = ast.build();

        let text = crate::format::format(&ast);
        let back = crate::parse::parse(&text).expect("formatted DSL flow must re-parse");
        assert_eq!(ast, back, "round-trip mismatch.\n--- text ---\n{text}");
    }

    /// Regression for the `flat`-without-`collect` round-trip break: a degenerate `Each` (flat set, no
    /// collect target) has no native text form, so it must fall through to the `@json` escape and still
    /// round-trip. Built straight through the documented `.flat()` setter.
    #[test]
    fn each_flat_without_collect_round_trips_via_json_escape() {
        let ast = flow(|b| {
            b.each("i", var("xs"), |e| {
                e.flat();
                e.body(|b| {
                    b.call("n", [var("i")]);
                });
            });
        });
        // The node really is the degenerate shape.
        assert!(matches!(
            &ast.body[0],
            Node::Each {
                collect: None,
                flat: true,
                ..
            }
        ));
        let text = crate::format::format(&ast);
        assert!(
            text.contains("@json"),
            "flat-without-collect each should escape to @json, got:\n{text}"
        );
        let back = crate::parse::parse(&text).expect("must re-parse");
        assert_eq!(ast, back);
    }

    /// The serde `kind` tag of a node built via the DSL.
    fn kind_of(node: &Node) -> String {
        serde_json::to_value(node).unwrap()["kind"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Build a block via `build` and return the single node it pushed.
    fn one(build: impl FnOnce(&mut Block)) -> Node {
        let mut b = Block::new();
        build(&mut b);
        b.into_body().pop().expect("builder pushed a node")
    }

    /// **The builder's drift guard.** Every `Node` kind in the catalog SSOT
    /// ([`crate::schema::node_kind_catalog`]) must be constructible through the DSL — if the
    /// add-a-node checklist introduces a new variant, this test fails until the DSL grows a
    /// constructor for it. Parallel to `skill_in_sync`/`skill_docs_in_sync`, but for the Rust surface.
    #[test]
    fn dsl_covers_every_node_kind() {
        let nodes = [
            // Expression constructors (free functions).
            call("op", []),
            var("x"),
            lit(1),
            expr("a + 1", [("a", var("a"))]),
            fmt("x"),
            jq(".a", var("r")),
            parse_as(var("r"), "f64"),
            thing(ThingKind::File, Selector::Path("p".into())),
            peek("x"),
            // Statement builders (Block methods).
            one(|b| {
                b.bind("x", lit(1));
            }),
            one(|b| {
                b.memo("m", call("n", []));
            }),
            one(|b| {
                b.ret(lit(1));
            }),
            one(|b| {
                b.assert(var("c"));
            }),
            one(|b| {
                b.verify(call("bash", []), lit("ok"));
            }),
            one(|b| {
                b.ctx_append("c", ["y"]);
            }),
            one(|b| {
                b.when(var("c"), |b| {
                    b.call("n", []);
                });
            }),
            one(|b| {
                b.unless(var("c"), |b| {
                    b.call("n", []);
                });
            }),
            one(|b| {
                b.seq(|b| {
                    b.call("n", []);
                });
            }),
            one(|b| {
                b.pipe([call("a", []), call("bee", [])]);
            }),
            one(|b| {
                b.confirm("ok?", |b| {
                    b.call("n", []);
                });
            }),
            one(|b| {
                b.throttle("t", 1, 1000, |b| {
                    b.call("n", []);
                });
            }),
            one(|b| {
                b.debounce("d", 100, |b| {
                    b.call("n", []);
                });
            }),
            one(|b| {
                b.each("i", var("xs"), |e| {
                    e.body(|b| {
                        b.call("n", [var("i")]);
                    });
                });
            }),
            one(|b| {
                b.repeat(1, |r| {
                    r.body(|b| {
                        b.call("n", []);
                    });
                });
            }),
            one(|b| {
                b.loop_for(100, |l| {
                    l.body(|b| {
                        b.call("n", []);
                    });
                });
            }),
            one(|b| {
                b.retry(1, |r| {
                    r.body(|b| {
                        b.call("n", []);
                    });
                });
            }),
            one(|b| {
                b.parallel(|p| {
                    p.branch("l", |b| {
                        b.call("n", []);
                    });
                });
            }),
            one(|b| {
                b.race(100, |r| {
                    r.branch("a", |b| {
                        b.call("n", []);
                    });
                });
            }),
            one(|b| {
                b.await_("src", |_| {});
            }),
            one(|b| {
                b.ctx("c", |c| {
                    c.include(["x"]);
                });
            }),
            one(|b| {
                b.try_(|t| {
                    t.body(|b| {
                        b.call("n", []);
                    });
                    t.handler(|b| {
                        b.call("h", []);
                    });
                });
            }),
            one(|b| {
                b.match_(var("s"), |m| {
                    m.case(lit(1), |b| {
                        b.call("n", []);
                    });
                });
            }),
            one(|b| {
                b.route(call("sel", []), |r| {
                    r.case("a", |b| {
                        b.call("n", []);
                    });
                });
            }),
            one(|b| {
                b.fallback(|f| {
                    f.branch(|b| {
                        b.call("n", []);
                    });
                });
            }),
            one(|b| {
                b.timeout(100, |w| {
                    w.body(|b| {
                        b.call("n", []);
                    });
                });
            }),
            one(|b| {
                b.budget(2, |w| {
                    w.body(|b| {
                        b.call("n", []);
                    });
                });
            }),
            one(|b| {
                b.scope(|s| {
                    s.acquire("h", call("lock", []));
                    s.body(|b| {
                        b.call("n", []);
                    });
                    s.finally(|b| {
                        b.call("release", []);
                    });
                });
            }),
            one(|b| {
                b.saga(|s| {
                    s.step(|st| {
                        st.body(|b| {
                            b.call("charge", []);
                        });
                        st.undo(|b| {
                            b.call("refund", []);
                        });
                    });
                });
            }),
            one(|b| {
                b.once("send-welcome", |w| {
                    w.body(|b| {
                        b.call("send_email", []);
                    });
                });
            }),
            one(|b| {
                b.checkpoint("phase-1");
            }),
        ];

        let built: std::collections::BTreeSet<String> = nodes.iter().map(kind_of).collect();
        let catalog: std::collections::BTreeSet<String> = crate::schema::node_kind_catalog()
            .lines()
            .skip(2) // header rows
            .filter_map(|l| l.split('`').nth(1).map(str::to_string))
            .collect();

        assert_eq!(
            built, catalog,
            "DSL must construct every catalog node kind.\nmissing from DSL: {:?}\nextra in DSL: {:?}",
            catalog.difference(&built).collect::<Vec<_>>(),
            built.difference(&catalog).collect::<Vec<_>>(),
        );
        assert_eq!(built.len(), 40, "expected all 40 node kinds");
    }
}
