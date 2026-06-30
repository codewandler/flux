//! The multi-agent **Program** layer: a `.flux` file may declare a whole app — agents, channels,
//! datasources, triggers, and journeys — not just a single flow. These are **pure-data declarations**
//! (L0, strings + an opaque `settings` JSON map); the L3 engine and the L6 `flux-app` host give them
//! their runtime *meaning* (model/datasource/channel wiring, the event bus, the scheduler). This keeps
//! the multi-agent vision coherent without expanding the language core: **orchestration is an op-pack**
//! (`ask`/`send`/`emit`/`spawn`), so this layer needs **zero new node kinds**.
//!
//! A program is authored in **native flux-lang text** ([`crate::parse::parse_program`], reached via
//! [`Module::parse_str`]); a bare single-flow file still loads, wrapping a lone [`DraftAst`]. "User
//! input is just an event": a trigger's `on` label shares the event-label space with
//! [`crate::ast::Node::Await`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ast::{DraftAst, Param};
use crate::error::Result;

use flux_spec::{Effect, Idempotency, Risk};

/// An agent: an identity plus the model / tool / datasource access surface it runs with. A superset
/// of an orchestration role; the L3 engine resolves the names to concrete capabilities.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct AgentDecl {
    pub name: String,
    /// The model this agent plans with (engine-resolved; `None` = the host default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The op/tool names this agent may call.
    #[serde(default)]
    pub tools: Vec<String>,
    /// The datasource names this agent may query.
    #[serde(default)]
    pub datasources: Vec<String>,
    /// A human-readable role description (seeds the agent's system framing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// An opaque settings bag — the L0 layer carries it verbatim; the engine interprets it.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub settings: serde_json::Value,
}

/// A channel: a named I/O surface the app listens and/or sends on (CLI, HTTP, Slack, …). The `kind`
/// selects the L6 channel runtime; `settings` configures it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ChannelDecl {
    pub name: String,
    /// The channel runtime to use (e.g. `cli`, `http`, `slack`).
    pub kind: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub settings: serde_json::Value,
}

/// A datasource: a named knowledge index an agent can query. The `kind` selects the L6 ingester
/// (`markdown`, `openapi`, …); `path` points at the source; `settings` configures it. Like the other
/// decls this is pure data — the host (`flux-capabilities`) gives it runtime meaning.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct DatasourceDecl {
    pub name: String,
    /// The ingester to use (e.g. `markdown`, `openapi`). In native text this defaults to the decl name.
    pub kind: String,
    /// The source location (a directory for `markdown`, a spec file for `openapi`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub settings: serde_json::Value,
}

/// A trigger: an event→action binding. `on` is an event label (sharing the space with `Node::Await`);
/// when it fires the named `run` journey/flow executes, optionally as a specific `agent`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct TriggerDecl {
    pub name: String,
    /// The event label that fires this trigger (e.g. `user_input`, `cron:nightly`, a channel name).
    pub on: String,
    /// The journey (or flow) name to run when the trigger fires.
    pub run: String,
    /// The agent to run it as, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// A journey: a named flow embedding a [`DraftAst`], run by the existing interpreter unchanged.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct JourneyDecl {
    pub name: String,
    /// The agent that owns this journey, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The flow body — an ordinary Draft AST.
    #[serde(default)]
    pub flow: DraftAst,
}

/// Execution limits declared on a composite op. The language carries these as pure metadata; hosts
/// decide how much of the limit surface they enforce.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct CompositeLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatches: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_chars: Option<u64>,
}

/// Metadata that makes a Flux-Lang composite op look like a normal operation to analysis and the
/// planner catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompositeOpMeta {
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_composite_risk")]
    #[schemars(with = "String")]
    pub risk: Risk,
    #[serde(default = "default_composite_idempotency")]
    #[schemars(with = "String")]
    pub idempotency: Idempotency,
    #[serde(default)]
    #[schemars(with = "Vec<String>")]
    pub effects: Vec<Effect>,
    #[serde(default = "default_composite_expose")]
    pub expose: bool,
    #[serde(default)]
    pub limits: CompositeLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
}

impl Default for CompositeOpMeta {
    fn default() -> Self {
        Self {
            description: String::new(),
            risk: default_composite_risk(),
            idempotency: default_composite_idempotency(),
            effects: Vec::new(),
            expose: default_composite_expose(),
            limits: CompositeLimits::default(),
            view: None,
        }
    }
}

fn default_composite_risk() -> Risk {
    Risk::Low
}

fn default_composite_idempotency() -> Idempotency {
    Idempotency::Idempotent
}

fn default_composite_expose() -> bool {
    true
}

/// A module-local operation implemented as a scoped Flux-Lang body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct CompositeOpDecl {
    pub name: String,
    #[serde(default)]
    pub params: Vec<Param>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<crate::ast::TypeRef>,
    #[serde(default)]
    pub meta: CompositeOpMeta,
    #[serde(default)]
    pub body: DraftAst,
}

/// A whole multi-agent program (a module): agents, channels, triggers, journeys, and any top-level
/// flows. Every field defaults to empty, so a minimal program is valid.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Program {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub agents: Vec<AgentDecl>,
    #[serde(default)]
    pub channels: Vec<ChannelDecl>,
    #[serde(default)]
    pub datasources: Vec<DatasourceDecl>,
    #[serde(default)]
    pub triggers: Vec<TriggerDecl>,
    #[serde(default)]
    pub journeys: Vec<JourneyDecl>,
    /// Module-local operations implemented by composing existing Flux-Lang ops.
    #[serde(default)]
    pub ops: Vec<CompositeOpDecl>,
    /// Top-level flows (not owned by a journey) — e.g. an entrypoint a host may run directly.
    #[serde(default)]
    pub flows: Vec<DraftAst>,
}

impl Program {
    /// Resolve a journey or top-level flow by name to its `DraftAst`.
    pub fn flow_named(&self, name: &str) -> Option<&DraftAst> {
        self.journeys
            .iter()
            .find(|j| j.name == name)
            .map(|j| &j.flow)
            .or_else(|| self.flows.iter().find(|f| f.name.as_deref() == Some(name)))
    }
}

// ---------------------------------------------------------------------------
// Secret references
// ---------------------------------------------------------------------------

/// The reserved settings key marking an **unresolved secret reference**: `{"$secret":"ENV_NAME"}`. A
/// `secret "NAME"` in native text lowers (in the pure parser) to this marker; the host resolves it from
/// the environment once at load (`flux_app::resolve_secrets`) — plaintext never lives in a committed
/// `.flux`. This is the single secret mechanism across the module layer.
pub const SECRET_KEY: &str = "$secret";

/// Build a secret-reference marker for the environment variable `name`.
pub fn secret_marker(name: &str) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert(
        SECRET_KEY.to_string(),
        serde_json::Value::String(name.to_string()),
    );
    serde_json::Value::Object(m)
}

/// If `v` is exactly a secret marker (`{"$secret":"NAME"}`), return `NAME`.
pub fn as_secret_ref(v: &serde_json::Value) -> Option<&str> {
    let obj = v.as_object()?;
    if obj.len() == 1 {
        if let Some(serde_json::Value::String(name)) = obj.get(SECRET_KEY) {
            return Some(name);
        }
    }
    None
}

/// A loaded `.flux` module: either a single bare flow or a full multi-agent program. The loader
/// sniffs the shape so a host can accept both `foo.flux` (one flow) and `app.flux` (a program). Both
/// are **native flux-lang text** — module declarations (`agent`/`channel`/`datasource`/`trigger`/
/// `journey`) mark a program; a lone `flow` header is a bare flow.
#[derive(Debug, Clone, PartialEq)]
pub enum Module {
    /// A bare single-flow file (a lone `DraftAst`).
    Flow(DraftAst),
    /// A multi-agent program.
    Program(Program),
}

impl Module {
    /// Load a `.flux` module from native flux-lang text, sniffing whether it is a bare flow or a
    /// multi-agent program. (The program/app layer used to be JSON; it is now native text — see
    /// [`crate::parse::parse_program`].)
    pub fn parse_str(s: &str) -> Result<Module> {
        crate::parse::parse_program(s)
    }

    /// The [`Program`] if this module is one, else `None` (a bare flow is not implicitly a program).
    pub fn program(&self) -> Option<&Program> {
        match self {
            Module::Program(p) => Some(p),
            Module::Flow(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_flow_loads_as_a_single_flow() {
        let m = Module::parse_str("flow greet\n  return null").unwrap();
        match m {
            Module::Flow(f) => assert_eq!(f.name.as_deref(), Some("greet")),
            Module::Program(_) => panic!("a bare flow must not sniff as a program"),
        }
    }

    #[test]
    fn a_document_with_agents_loads_as_a_program() {
        // Native flux-lang text: module declarations make it a program; `channel cli` defaults its
        // `kind` to the decl name.
        let src = "\
agent triager
  model \"claude-opus-4-8\"
  tools [read, grep]

channel cli

trigger t
  on \"user_input\"
  run handle

journey handle
  flow
    return null
";
        let Module::Program(p) = Module::parse_str(src).unwrap() else {
            panic!("module declarations must sniff as a program");
        };
        assert_eq!(p.agents.len(), 1);
        assert_eq!(p.agents[0].model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(p.agents[0].tools, vec!["read", "grep"]);
        assert_eq!(p.channels[0].kind, "cli");
        assert_eq!(p.triggers[0].on, "user_input");
        assert!(p.flow_named("handle").is_some(), "journey resolves by name");
    }

    #[test]
    fn program_round_trips_through_json() {
        let p = Program {
            name: Some("a".into()),
            agents: vec![AgentDecl {
                name: "x".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: Program = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }
}
