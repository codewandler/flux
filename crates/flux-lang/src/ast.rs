//! The pure language contracts: the Draft AST the LLM emits, the typed HIR the analyzer produces,
//! the physical plan the optimizer produces, the value model, semantic effects, and the run-event
//! trace. No IO and no flux-runtime/provider dependency — these types are shared by every phase and
//! by the UI projections.
//!
//! The JSON shape mirrors the compact syntax: a `call` node is `{"kind":"call","op":...,"args":[…]}`,
//! a variable reference is `{"kind":"var","name":"draft"}`, and so on.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

macro_rules! string_id {
    ($($(#[$m:meta])* $name:ident),* $(,)?) => {$(
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl From<String> for $name {
            fn from(s: String) -> Self { Self(s) }
        }
        impl From<&str> for $name {
            fn from(s: &str) -> Self { Self(s.to_string()) }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    )*};
}

string_id! {
    /// A session symbol name (the `$draft` in source is stored without the `$`).
    SymbolName,
    /// The identity of a stored immutable value.
    ValueId,
    /// The identity of a resolved external thing.
    ThingId,
    /// A saved flow's identity.
    FlowId,
    /// A single execution's identity.
    RunId,
    /// A step (op invocation) within a run.
    StepId,
}

/// A node's index within a flow's flattened node table (used by the physical plan).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u32);

// ---------------------------------------------------------------------------
// Types and effects
// ---------------------------------------------------------------------------

/// A (deliberately small) type reference. The analyzer checks op signatures against these; richer
/// structural typing can grow here later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TypeRef {
    /// The top type — matches anything (used before inference).
    Any,
    Bool,
    Number,
    String,
    /// A homogeneous list.
    List(Box<TypeRef>),
    /// A named/registered type (struct schema, op output type, …).
    Named(String),
}

impl TypeRef {
    /// A compact human label (`List<String>`, `Ticket`, …) — the type-hint shown in renders and
    /// stored alongside a bound symbol.
    pub fn label(&self) -> String {
        match self {
            TypeRef::Any => "Any".to_string(),
            TypeRef::Bool => "Bool".to_string(),
            TypeRef::Number => "Number".to_string(),
            TypeRef::String => "String".to_string(),
            TypeRef::List(inner) => format!("List<{}>", inner.label()),
            TypeRef::Named(n) => n.clone(),
        }
    }
}

/// A first-class *semantic* effect, declared on operations. Distinct from the host-resource
/// [`flux_spec::Effect`] (Read/Write/Network/…): a `FlowEffect` expresses execution *meaning*
/// (this op sends mail, costs money, touches a calendar) and lowers onto the host effect + a policy
/// action via [`FlowEffect::lower`](crate). Policy decides allow / deny / require-approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FlowEffect {
    /// No effect — deterministic, side-effect free.
    Pure,
    /// Reads external state.
    Read,
    /// Invokes a model (non-deterministic unless cached).
    Model,
    /// General network egress.
    Network,
    /// Writes to the filesystem.
    WriteFile,
    /// Writes to a database / persistent store.
    WriteDb,
    /// Sends something externally (email, message, webhook).
    SendExternal,
    /// Irreversibly deletes.
    Delete,
    /// Moves money.
    Money,
    /// Mutates a calendar.
    Calendar,
    /// Produces output a human will see.
    HumanVisible,
}

/// How visible a session symbol is to the model when projecting `view(Session)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Referenced naturally and shown in the projected view.
    Visible,
    /// Stored but not shown by default.
    Hidden,
    /// Always shown.
    Pinned,
    /// Only accessible via explicit search.
    Expired,
    /// Never shown to the model unless explicitly required.
    Private,
}

impl Visibility {
    /// The stable lowercase tag (used as a storage key).
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Visible => "visible",
            Visibility::Hidden => "hidden",
            Visibility::Pinned => "pinned",
            Visibility::Expired => "expired",
            Visibility::Private => "private",
        }
    }

    /// Parse from the stable tag.
    pub fn from_tag(s: &str) -> Option<Self> {
        Some(match s {
            "visible" => Visibility::Visible,
            "hidden" => Visibility::Hidden,
            "pinned" => Visibility::Pinned,
            "expired" => Visibility::Expired,
            "private" => Visibility::Private,
            _ => return None,
        })
    }

    /// Whether a symbol with this visibility appears in the default model-facing view.
    pub fn is_shown(self) -> bool {
        matches!(self, Visibility::Visible | Visibility::Pinned)
    }
}

// ---------------------------------------------------------------------------
// Values and things
// ---------------------------------------------------------------------------

/// An immutable runtime value produced by an operation. Stored in the value store and referenced by
/// symbol; only the runtime dereferences it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    List(Vec<Value>),
    /// A struct with deterministically-ordered fields.
    Struct(BTreeMap<String, Value>),
    /// A resolved external thing.
    Thing(ResolvedThing),
    /// A reference to another stored value.
    Ref(ValueId),
}

impl Value {
    /// Project to *natural* JSON — a [`Value::String`] becomes a JSON string, not the tagged
    /// `{"string": …}` serde form. This is the shape an op input expects, so the interpreter injects
    /// a stored value into a call argument through here (the symbols-over-values thesis, executed).
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            Value::Null => J::Null,
            Value::Bool(b) => J::Bool(*b),
            Value::Number(n) => serde_json::Number::from_f64(*n)
                .map(J::Number)
                .unwrap_or(J::Null),
            Value::String(s) => J::String(s.clone()),
            Value::List(items) => J::Array(items.iter().map(Value::to_json).collect()),
            Value::Struct(fields) => J::Object(
                fields
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_json()))
                    .collect(),
            ),
            // A resolved thing surfaces as its display string; a stored-value ref as its id.
            Value::Thing(t) => J::String(t.display.clone()),
            Value::Ref(id) => J::String(id.0.clone()),
        }
    }

    /// Build a [`Value`] from *natural* JSON — the inverse of [`Value::to_json`]. Used when iterating
    /// a list (`each`) to store each element as a `Value`, and when gathering per-iteration results.
    /// A non-finite or otherwise-unrepresentable number becomes [`Value::Null`].
    pub fn from_json(v: &serde_json::Value) -> Self {
        use serde_json::Value as J;
        match v {
            J::Null => Value::Null,
            J::Bool(b) => Value::Bool(*b),
            J::Number(n) => n.as_f64().map(Value::Number).unwrap_or(Value::Null),
            J::String(s) => Value::String(s.clone()),
            J::Array(items) => Value::List(items.iter().map(Value::from_json).collect()),
            J::Object(fields) => Value::Struct(
                fields
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::from_json(v)))
                    .collect(),
            ),
        }
    }
}

/// The kind of an addressable external object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
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
    /// A domain-specific kind.
    Custom(String),
}

/// How a thing is addressed before resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Selector {
    Id(String),
    Name(String),
    Path(String),
    Query(String),
    Key(String),
}

/// An unresolved reference to an external object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ThingRef {
    pub kind: ThingKind,
    pub selector: Selector,
}

/// A thing resolved to an exact identity. No side effect may execute until every required thing is
/// resolved unambiguously.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedThing {
    pub id: ThingId,
    pub kind: ThingKind,
    pub display: String,
    /// Where the resolution came from (resolver name / source system).
    pub source: String,
    /// Resolver confidence in [0, 1].
    pub confidence: f32,
}

// ---------------------------------------------------------------------------
// Draft AST
// ---------------------------------------------------------------------------

/// A typed flow parameter (`$ticket: Ticket`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Param {
    pub name: SymbolName,
    pub ty: TypeRef,
}

/// A node in the Draft AST the LLM emits. Expressions and statements share one enum; the analyzer
/// enforces where each may appear.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Node {
    /// Invoke a registered operation with argument expressions.
    Call {
        op: String,
        #[serde(default)]
        args: Vec<Node>,
    },
    /// Bind the result of an expression to a symbol.
    Bind {
        name: SymbolName,
        value: Box<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ty: Option<TypeRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect: Option<FlowEffect>,
    },
    /// Conditional control flow.
    When {
        cond: Box<Node>,
        #[serde(default)]
        then: Vec<Node>,
        #[serde(default)]
        otherwise: Vec<Node>,
    },
    /// A bounded loop (`max` is required; the analyzer rejects unbounded loops).
    Repeat {
        max: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<Box<Node>>,
        #[serde(default)]
        body: Vec<Node>,
        /// Optional symbol bound to a [`Value::List`] of each iteration's last result.
        /// Mirrors `each`'s `collect` field for counter-driven loops.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        collect: Option<SymbolName>,
    },
    /// Map a list value through a body (list-driven loop; `repeat` stays counter-driven). Each element
    /// is bound to `as`; an optional `collect` symbol gathers the per-iteration results into a list.
    Each {
        /// The list to iterate (an expression yielding a [`Value::List`]).
        #[serde(rename = "in")]
        source: Box<Node>,
        /// The symbol bound to each element inside the body.
        #[serde(rename = "as")]
        item: SymbolName,
        #[serde(default)]
        body: Vec<Node>,
        /// Optional symbol bound to a [`Value::List`] of each iteration's result.
        /// When `flat` is `true` the collected values are flattened one level
        /// (each iteration must produce a list; the results are concatenated).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        collect: Option<SymbolName>,
        /// If `true` and `collect` is set, flatten the per-iteration results
        /// (each must be a list) into one combined list instead of a list-of-lists.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        flat: bool,
    },
    /// A boolean guard: aborts the flow with an error if the condition is falsey.
    Assert {
        cond: Box<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// A chain of calls where each step's output is fed as the first argument of the next.
    Pipe {
        #[serde(default)]
        steps: Vec<Node>,
        /// Optional symbol bound to the chain's final result.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bind: Option<SymbolName>,
    },
    /// A sequential block; runs its body in order. Optionally binds the block's final result.
    Seq {
        #[serde(default)]
        body: Vec<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bind: Option<SymbolName>,
    },
    /// Like `bind`, but pinned across turns: if the symbol is already resolved for this session, skip
    /// execution and reuse the cached value (compute-once-per-session, keyed on symbol name).
    Memo {
        name: SymbolName,
        value: Box<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ty: Option<TypeRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect: Option<FlowEffect>,
    },
    /// Concurrent fan-out: run independent branches, binding each branch's result to its name.
    Parallel {
        #[serde(default)]
        branches: Vec<Branch>,
    },
    /// Pause until an external event/input arrives.
    Await {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        binding: Option<SymbolName>,
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        as_type: Option<TypeRef>,
    },
    /// Retry a body on failure with optional backoff. Fatal errors (policy denial, unknown op) are
    /// never retried. `backoff` may be `"none"` | `"linear"` | `"exponential"`.
    Retry {
        max: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backoff: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delay_ms: Option<u64>,
        #[serde(default)]
        body: Vec<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bind: Option<SymbolName>,
    },
    /// Structured error handling: run `body`; on failure bind the error string to `catch` and run
    /// `handler`. If the handler also errors, propagate that error.
    Try {
        #[serde(default)]
        body: Vec<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        catch: Option<SymbolName>,
        #[serde(default)]
        handler: Vec<Node>,
    },
    /// Explicit human-in-the-loop gate. Calls the existing `Approver` — `--yes` and TUI modal
    /// handle it automatically. Body only runs on approval; on denial the node errors.
    /// `risk` may be `"low"` | `"medium"` | `"high"` | `"critical"`.
    Confirm {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        risk: Option<String>,
        #[serde(default)]
        body: Vec<Node>,
    },
    /// Time-bounded iteration. `for_ms` is required (the analyzer rejects unbounded loops).
    /// `every_ms` is the inter-iteration sleep (0 = tight). `until` is an early-exit condition.
    Loop {
        for_ms: u64,
        #[serde(default)]
        every_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<Box<Node>>,
        #[serde(default)]
        body: Vec<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bind: Option<SymbolName>,
    },
    /// First-wins concurrency: run branches in parallel and return as soon as the first
    /// succeeds. `timeout_ms` is required; if no branch succeeds within it the node errors.
    /// `bind` names the symbol that receives the winning branch's result.
    Race {
        timeout_ms: u64,
        #[serde(default)]
        branches: Vec<Branch>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bind: Option<SymbolName>,
    },
    /// Rate-limit body execution: at most `max` dispatches per `window_ms` sliding window.
    /// The token bucket is tracked in the session store keyed by `name`; plan authors declare intent,
    /// runtime enforces. `name` must be unique within a session to avoid bucket collisions.
    Throttle {
        /// Stable bucket name — keyed in the session store so the limit persists across turns.
        name: String,
        max: u32,
        window_ms: u64,
        #[serde(default)]
        body: Vec<Node>,
    },
    /// Coalesce rapid re-invocations: wait `wait_ms` after the last trigger before running body.
    /// In a `loop`/`watch` context the body only executes when things have settled.
    /// `name` is used as a stable key so debounce state survives across turns.
    Debounce {
        /// Stable key name for this debounce site.
        name: String,
        wait_ms: u64,
        #[serde(default)]
        body: Vec<Node>,
    },
    /// Negated conditional: run `body` only when `cond` is falsey. Sugar for `when !cond`;
    /// the body may contain any nodes (reads, writes, sub-plans — anything).
    Unless {
        cond: Box<Node>,
        #[serde(default)]
        body: Vec<Node>,
    },
    /// Run a command and assert its output matches an expected pattern; abort the flow with a
    /// structured error if it does not. `cmd` is any node that produces a string (typically a
    /// `bash` call); `expect` is a substring or regex the output must contain.
    Verify {
        cmd: Box<Node>,
        expect: Box<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// End the flow with a value.
    Return { value: Box<Node> },
    /// Read the current in-session value of a named symbol without any filesystem IO.
    /// Returns the symbol's stored value, or null if the symbol is not yet bound.
    Peek { name: SymbolName },
    /// Reference a bound symbol.
    Var { name: SymbolName },
    /// A literal value (raw JSON, as written in the AST by the compiler front-end).
    Lit { value: serde_json::Value },
    /// A reference to an external thing.
    Thing { thing: ThingRef },

    /// Pure inline arithmetic. `formula` is a safe whitelist expression (`+`, `-`, `*`, `/`,
    /// `round(x,n)`, `abs`, `min(a,b)`, `max(a,b)`) over named variables. `vars` maps variable
    /// names to node expressions (only `Lit` and `Var` are valid). No IO, no approval gate.
    /// Example: `expr("price * 2", {"price": $btc})`.
    Expr {
        formula: String,
        #[serde(default)]
        vars: std::collections::BTreeMap<String, Box<Node>>,
    },

    /// Pure string interpolation. `template` is a string with `{name}` placeholders substituted
    /// from already-bound session symbols (same `{name}`/`{{name}}` syntax as `Lit` interpolation).
    /// No IO, no approval gate. Example: `fmt("BTC: {price} | Double: {doubled}")`.
    Fmt { template: String },

    /// Pure JSON path extraction. `path` is a dot-path string (e.g. `".bitcoin.usd"` or
    /// `"results[0].value"`) applied to the JSON content of `input` (a `Var` or `Lit` node).
    /// No IO, no approval gate. Example: `jq(".bitcoin.usd", $raw)`.
    Jq { path: String, input: Box<Node> },

    /// Pure type coercion. Converts the string result of a `jq` or `fmt` node into a typed
    /// value. `as_type` is one of `"f64"`, `"i64"`, `"bool"`, `"json"`, `"string"`.
    /// No IO, no approval gate. Example: `parse(jq(".price", $raw), as: "f64")`.
    Parse {
        value: Box<Node>,
        #[serde(rename = "as")]
        as_type: String,
    },

    /// Build a bounded, budgeted **context pack** from existing symbols. Resolves `include` (minus
    /// `exclude`) to its members, then — when `budget` is set — shrinks the pack *at evaluation* by
    /// visibility tier then declared order until within the char budget, recording any dropped members
    /// in the run trace. Produces a `Ctx` value bound to `name`. Pure: it selects and labels existing
    /// values, performing no IO (the load-bearing elevation of PRD §13 explicit context management).
    Ctx {
        name: SymbolName,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        purpose: Option<String>,
        #[serde(default)]
        include: Vec<SymbolName>,
        #[serde(default)]
        exclude: Vec<SymbolName>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget: Option<u64>,
    },

    /// Accrete more symbols into an existing context pack (the `+=` marker). Immutably rebinds `ctx`
    /// to a *new* `Ctx` value (preserving the audit chain `$pack@1 → @2`) with `add` appended, then
    /// re-applies the pack's budget. Pure.
    CtxAppend {
        ctx: SymbolName,
        #[serde(default)]
        add: Vec<SymbolName>,
    },

    /// Multi-way **exhaustive** branch: evaluate `subject` (a literal or bound symbol), then run the
    /// body of the first `case` whose `value` equals it — by JSON equality, so a *string* subject does
    /// not equal a *numeric* literal. If none match, run `default`. A deterministic replacement for
    /// chains of `when`. To branch on an op's result, bind it first (`$s = call(); match $s {…}`) or
    /// use `route`. The analyzer requires at least one case; at runtime an unmatched subject with no
    /// `default` is an error — the exhaustiveness guard-rail.
    Match {
        subject: Box<Node>,
        #[serde(default)]
        cases: Vec<MatchCase>,
        #[serde(default)]
        default: Vec<Node>,
    },

    /// Model-routed branch — the signature *bounded non-determinism* primitive. Run `selector`
    /// (typically a `!model` op) to produce a label, then run the `case` whose `label` it names. The
    /// cases are fixed and analyzer-validated: the model chooses *which* declared branch runs, never
    /// *what*. Falls back to `default` when the label matches no case (an error if `default` is empty).
    Route {
        selector: Box<Node>,
        #[serde(default)]
        cases: Vec<RouteCase>,
        #[serde(default)]
        default: Vec<Node>,
    },

    /// Ordered "first that succeeds wins" selector: run each branch in `branches` in turn; the first
    /// that completes without error and yields a non-empty result wins and becomes the node's result.
    /// On a branch error (or empty result) the next is tried — so a *side-effecting* branch that
    /// returns empty will still fall through and the next branch also runs (attempts stream live, as
    /// in `try`/`retry`). If every branch errors, the last error propagates. Lighter than `try` for
    /// graceful degradation (cheap path → else expensive path). `bind` names the winning result.
    Fallback {
        #[serde(default)]
        branches: Vec<FallbackBranch>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bind: Option<SymbolName>,
    },

    /// Bound the wall-clock of a sub-flow: run `body` with a `ms` deadline. If it does not finish in
    /// time the node errors (an enclosing `try`/`retry` may catch it). A general reliability
    /// guard-rail you can wrap around anything. `bind` names the body's result.
    Timeout {
        ms: u64,
        #[serde(default)]
        body: Vec<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bind: Option<SymbolName>,
    },

    /// Cap the cost of a scope: run `body` but allow at most `limit` op dispatches within it (checked
    /// at statement boundaries — the body stops before a statement that would exceed the cap, and the
    /// node errors). A first-class cost guard-rail; v1 counts dispatches (token/money budgets are a
    /// later refinement). `bind` names the body's result.
    Budget {
        limit: u32,
        #[serde(default)]
        body: Vec<Node>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bind: Option<SymbolName>,
    },
}

/// One branch of a [`Node::Parallel`] fan-out: a named sub-flow whose final result is bound to
/// `name` after the branch completes. Branches should bind distinct names (the analyzer rejects
/// duplicates).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Branch {
    pub name: SymbolName,
    #[serde(default)]
    pub body: Vec<Node>,
}

/// One arm of a [`Node::Match`]: run `body` when the subject equals `value` (by JSON equality).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MatchCase {
    pub value: Node,
    #[serde(default)]
    pub body: Vec<Node>,
}

/// One arm of a [`Node::Route`]: run `body` when the selector yields the string `label`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RouteCase {
    pub label: String,
    #[serde(default)]
    pub body: Vec<Node>,
}

/// One branch of a [`Node::Fallback`]: a body tried in declared order until one succeeds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FallbackBranch {
    #[serde(default)]
    pub body: Vec<Node>,
}

/// The Draft AST: an optionally-named, parameterized flow with a body. May contain unresolved
/// symbols and thing references until the analyzer runs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct DraftAst {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub params: Vec<Param>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<TypeRef>,
    #[serde(default)]
    pub body: Vec<Node>,
}

// ---------------------------------------------------------------------------
// HIR
// ---------------------------------------------------------------------------

/// The typed High-level IR the analyzer produces: structurally like [`DraftAst`] but validated
/// (names resolved, types checked, effects gathered). Type annotations grow on the nodes in later
/// milestones; for now it carries the same body plus the gathered effect set.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HirFlow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub params: Vec<Param>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<TypeRef>,
    #[serde(default)]
    pub body: Vec<Node>,
    /// The union of effects declared by the ops this flow calls (drives risk + approval).
    #[serde(default)]
    pub effects: Vec<FlowEffect>,
}

// ---------------------------------------------------------------------------
// Physical plan
// ---------------------------------------------------------------------------

/// The optimizer's output: an ordered set of execution stages over the flow's node table.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PhysicalPlan {
    #[serde(default)]
    pub stages: Vec<Stage>,
}

/// One stage of execution. `ApprovalFence` is a *scheduling* marker (don't speculate past it); the
/// enforcement authority is always the dispatch approval gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Run one node.
    Sequential(NodeId),
    /// Run independent nodes concurrently (effects must commute).
    Parallel(Vec<NodeId>),
    /// Conditional sub-plan.
    Branch(BranchPlan),
    /// Bounded loop sub-plan.
    Repeat(RepeatPlan),
    /// Pause for an event/input.
    Await(AwaitPlan),
    /// A scheduling fence before a side-effecting node (approval is still enforced at dispatch).
    ApprovalFence(NodeId),
}

/// A conditional sub-plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BranchPlan {
    pub cond: NodeId,
    #[serde(default)]
    pub then: Vec<Stage>,
    #[serde(default)]
    pub otherwise: Vec<Stage>,
}

/// A bounded-loop sub-plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepeatPlan {
    pub max: u32,
    #[serde(default)]
    pub body: Vec<Stage>,
}

/// An await sub-plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AwaitPlan {
    pub node: NodeId,
}

// ---------------------------------------------------------------------------
// Run-event trace
// ---------------------------------------------------------------------------

/// An immutable execution trace event. The complement to the security/audit `EvidenceLog`: this is
/// the *replayable* record of what ran (input hashes, output refs), readable like a program trace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RunEvent {
    FlowStarted {
        run: RunId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        flow: Option<FlowId>,
    },
    ThingResolved {
        thing: ThingRef,
        resolved: ResolvedThing,
    },
    StepStarted {
        step: StepId,
        op: String,
        input_hash: String,
    },
    StepSucceeded {
        step: StepId,
        output: ValueId,
    },
    StepFailed {
        step: StepId,
        error: String,
    },
    ApprovalRequested {
        step: StepId,
        effects: Vec<FlowEffect>,
    },
    ApprovalGranted {
        step: StepId,
    },
    /// The flow suspended on an `await`, to be resumed on a later turn.
    Awaiting {
        run: RunId,
        node: NodeId,
    },
    FlowReturned {
        value: ValueId,
    },
    /// A `ctx`/`ctx_append` pack was budgeted at evaluation: `kept` members were retained and
    /// `dropped` members removed to fit `budget` (chars, v1 heuristic). The replayable record of an
    /// intentional context shrink.
    CtxShrunk {
        ctx: String,
        #[serde(default)]
        kept: Vec<String>,
        #[serde(default)]
        dropped: Vec<String>,
        budget: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative Draft AST round-trips through JSON unchanged.
    #[test]
    fn draft_ast_json_round_trips() {
        let ast = DraftAst {
            name: Some("Reply".into()),
            params: vec![Param {
                name: "ticket".into(),
                ty: TypeRef::Named("Ticket".into()),
            }],
            returns: Some(TypeRef::Named("Result".into())),
            body: vec![
                Node::Bind {
                    name: "draft".into(),
                    value: Box::new(Node::Call {
                        op: "email.draft".into(),
                        args: vec![
                            Node::Var {
                                name: "ticket".into(),
                            },
                            Node::Lit {
                                value: serde_json::json!("renewal"),
                            },
                        ],
                    }),
                    ty: Some(TypeRef::Named("Draft".into())),
                    effect: Some(FlowEffect::Model),
                },
                Node::When {
                    cond: Box::new(Node::Var {
                        name: "draft".into(),
                    }),
                    then: vec![Node::Return {
                        value: Box::new(Node::Call {
                            op: "send.email".into(),
                            args: vec![Node::Thing {
                                thing: ThingRef {
                                    kind: ThingKind::Person,
                                    selector: Selector::Name("John".into()),
                                },
                            }],
                        }),
                    }],
                    otherwise: vec![],
                },
            ],
        };

        let json = serde_json::to_value(&ast).unwrap();
        let back: DraftAst = serde_json::from_value(json).unwrap();
        assert_eq!(ast, back);
    }

    /// The expanded node kinds (`each`/`assert`/`pipe`/`seq`/`memo`/`parallel`) round-trip through
    /// JSON, including the `in`/`as` field renames and the `parallel` branch list.
    #[test]
    fn expanded_nodes_json_round_trip() {
        let ast = DraftAst {
            body: vec![
                Node::Each {
                    source: Box::new(Node::Lit {
                        value: serde_json::json!(["a", "b"]),
                    }),
                    item: "f".into(),
                    body: vec![Node::Call {
                        op: "read".into(),
                        args: vec![Node::Var { name: "f".into() }],
                    }],
                    collect: Some("contents".into()),
                    flat: false,
                },
                Node::Assert {
                    cond: Box::new(Node::Var {
                        name: "contents".into(),
                    }),
                    message: Some("must be non-empty".into()),
                },
                Node::Pipe {
                    steps: vec![
                        Node::Call {
                            op: "read".into(),
                            args: vec![Node::Lit {
                                value: serde_json::json!("x"),
                            }],
                        },
                        Node::Call {
                            op: "grep".into(),
                            args: vec![Node::Lit {
                                value: serde_json::json!("todo"),
                            }],
                        },
                    ],
                    bind: Some("hits".into()),
                },
                Node::Seq {
                    body: vec![Node::Call {
                        op: "read".into(),
                        args: vec![],
                    }],
                    bind: None,
                },
                Node::Memo {
                    name: "survey".into(),
                    value: Box::new(Node::Call {
                        op: "read".into(),
                        args: vec![],
                    }),
                    ty: Some(TypeRef::String),
                    effect: Some(FlowEffect::Read),
                },
                Node::Parallel {
                    branches: vec![
                        Branch {
                            name: "left".into(),
                            body: vec![Node::Call {
                                op: "read".into(),
                                args: vec![Node::Lit {
                                    value: serde_json::json!("l"),
                                }],
                            }],
                        },
                        Branch {
                            name: "right".into(),
                            body: vec![Node::Call {
                                op: "read".into(),
                                args: vec![Node::Lit {
                                    value: serde_json::json!("r"),
                                }],
                            }],
                        },
                    ],
                },
            ],
            ..Default::default()
        };

        let json = serde_json::to_value(&ast).unwrap();
        // The `in`/`as` renames are honored on the wire.
        assert_eq!(json["body"][0]["kind"], "each");
        assert_eq!(json["body"][0]["in"]["kind"], "lit");
        assert_eq!(json["body"][0]["as"], "f");
        assert_eq!(json["body"][5]["kind"], "parallel");
        assert_eq!(json["body"][5]["branches"][0]["name"], "left");

        let back: DraftAst = serde_json::from_value(json).unwrap();
        assert_eq!(ast, back);
    }

    /// `Value::from_json` is the inverse of `to_json` (round-trips natural JSON through the value model).
    #[test]
    fn value_from_json_round_trips() {
        let j = serde_json::json!({"a": "x", "b": [1.0, true, null]});
        assert_eq!(Value::from_json(&j).to_json(), j);
    }

    /// `Value::to_json` produces the natural JSON shape (a string is a JSON string, not the tagged
    /// serde form), recursing through lists and structs.
    #[test]
    fn value_to_json_is_natural() {
        assert_eq!(
            Value::String("hi".into()).to_json(),
            serde_json::json!("hi")
        );
        assert_eq!(Value::Number(3.0).to_json(), serde_json::json!(3.0));
        assert_eq!(Value::Bool(true).to_json(), serde_json::json!(true));
        assert_eq!(Value::Null.to_json(), serde_json::Value::Null);

        let mut fields = BTreeMap::new();
        fields.insert("a".to_string(), Value::String("x".into()));
        fields.insert("b".to_string(), Value::List(vec![Value::Number(1.0)]));
        assert_eq!(
            Value::Struct(fields).to_json(),
            serde_json::json!({"a": "x", "b": [1.0]})
        );
    }

    /// The `kind` tag and bare-string symbol names match the documented JSON shape.
    #[test]
    fn node_json_shape_is_stable() {
        let node = Node::Call {
            op: "send.email".into(),
            args: vec![Node::Var {
                name: "john".into(),
            }],
        };
        let json = serde_json::to_value(&node).unwrap();
        assert_eq!(json["kind"], "call");
        assert_eq!(json["op"], "send.email");
        assert_eq!(json["args"][0]["kind"], "var");
        assert_eq!(json["args"][0]["name"], "john");
    }

    /// The physical-plan and run-event types serialize cleanly (used by later phases).
    #[test]
    fn plan_and_events_round_trip() {
        let plan = PhysicalPlan {
            stages: vec![
                Stage::Parallel(vec![NodeId(0), NodeId(1)]),
                Stage::ApprovalFence(NodeId(2)),
                Stage::Sequential(NodeId(2)),
            ],
        };
        let back: PhysicalPlan =
            serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
        assert_eq!(plan, back);

        let ev = RunEvent::StepSucceeded {
            step: "s1".into(),
            output: "v8".into(),
        };
        let back: RunEvent = serde_json::from_value(serde_json::to_value(&ev).unwrap()).unwrap();
        assert_eq!(ev, back);
    }
}
