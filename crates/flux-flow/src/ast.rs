//! The pure language contracts: the Draft AST the LLM emits, the typed HIR the analyzer produces,
//! the physical plan the optimizer produces, the value model, semantic effects, and the run-event
//! trace. No IO and no flux-runtime/provider dependency — these types are shared by every phase and
//! by the UI projections.
//!
//! The JSON shape mirrors the compact syntax: a `call` node is `{"kind":"call","op":...,"args":[…]}`,
//! a variable reference is `{"kind":"var","name":"draft"}`, and so on.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

macro_rules! string_id {
    ($($(#[$m:meta])* $name:ident),* $(,)?) => {$(
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// A first-class *semantic* effect, declared on operations. Distinct from the host-resource
/// [`flux_spec::Effect`] (Read/Write/Network/…): a `FlowEffect` expresses execution *meaning*
/// (this op sends mail, costs money, touches a calendar) and lowers onto the host effect + a policy
/// action via [`FlowEffect::lower`](crate). Policy decides allow / deny / require-approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// The kind of an addressable external object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selector {
    Id(String),
    Name(String),
    Path(String),
    Query(String),
    Key(String),
}

/// An unresolved reference to an external object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: SymbolName,
    pub ty: TypeRef,
}

/// A node in the Draft AST the LLM emits. Expressions and statements share one enum; the analyzer
/// enforces where each may appear.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    },
    /// Pause until an external event/input arrives.
    Await {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        binding: Option<SymbolName>,
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        as_type: Option<TypeRef>,
    },
    /// End the flow with a value.
    Return { value: Box<Node> },
    /// Reference a bound symbol.
    Var { name: SymbolName },
    /// A literal value.
    Lit { value: Value },
    /// A reference to an external thing.
    Thing { thing: ThingRef },
}

/// The Draft AST: an optionally-named, parameterized flow with a body. May contain unresolved
/// symbols and thing references until the analyzer runs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
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
                                value: Value::String("renewal".into()),
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
